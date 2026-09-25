// The browser-specific connection store. Catalog CAS and future credential
// mutations share one IndexedDB transaction; package workers cannot open this
// origin's database.
const DATABASE = "smudgy-web";
const DATABASE_VERSION = 5;
const CATALOG_SCHEMA = 3;
const STORE = "connection-catalog";
const CREDENTIALS = "profile-credentials";
const LAYOUTS = "named-layouts";
const LAST_WINDOW = "last-window";
const BROWSER_SETTINGS = "browser-settings";
const KEY = "current";
const EMPTY = { generation: 0, servers: [] };
const credentialKey = (server, profile) => JSON.stringify([server, profile]);

function catalogFromRecord(value) {
  if (value === undefined) return EMPTY;
  const record = JSON.parse(value);
  if (record.schema === 2 || record.schema === CATALOG_SCHEMA) {
    if (!record.catalog || !Number.isSafeInteger(record.catalog.generation)
      || !Array.isArray(record.catalog.servers)) {
      throw new Error('Saved connection catalog is malformed');
    }
    return record.catalog;
  }
  // Version 1 stored the catalog directly, with no envelope. Do not rewrite
  // on read: the next CAS save wraps it atomically, preserving its generation.
  if (record.schema === undefined && Number.isSafeInteger(record.generation)
    && Array.isArray(record.servers)) return record;
  throw new Error('Saved connection catalog has an unsupported schema');
}

function openDatabase() {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE, DATABASE_VERSION);
    request.onupgradeneeded = () => {
      if (!request.result.objectStoreNames.contains(STORE)) {
        request.result.createObjectStore(STORE);
      }
      if (!request.result.objectStoreNames.contains(CREDENTIALS)) {
        request.result.createObjectStore(CREDENTIALS);
      }
      if (!request.result.objectStoreNames.contains(LAYOUTS)) {
        request.result.createObjectStore(LAYOUTS).createIndex('server', 'server');
      }
      if (!request.result.objectStoreNames.contains(LAST_WINDOW)) {
        request.result.createObjectStore(LAST_WINDOW);
      }
      if (!request.result.objectStoreNames.contains(BROWSER_SETTINGS)) {
        request.result.createObjectStore(BROWSER_SETTINGS);
      }
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("Could not open IndexedDB"));
  });
}

// One durable snapshot per browser origin. Keep at most one write in flight
// and one latest pending state while dragging panes. Across tabs, the last
// committed write wins; this is not a catalog of browser windows.
let activeWindowWrite;
let pendingWindowJson;

export async function loadLastWindow() {
  if (activeWindowWrite) await activeWindowWrite.catch(() => {});
  const db = await openDatabase();
  try {
    return await new Promise((resolve, reject) => {
      const transaction = db.transaction(LAST_WINDOW, 'readonly');
      const request = transaction.objectStore(LAST_WINDOW).get(KEY);
      let json = null;
      request.onsuccess = () => { json = request.result ?? null; };
      transaction.oncomplete = () => resolve(json);
      transaction.onabort = () => reject(transaction.error ?? new Error('Window read aborted'));
      transaction.onerror = () => reject(transaction.error ?? new Error('Window read failed'));
    });
  } finally {
    db.close();
  }
}

async function writeLastWindow(json) {
  const db = await openDatabase();
  try {
    await new Promise((resolve, reject) => {
      const transaction = db.transaction(LAST_WINDOW, 'readwrite');
      transaction.objectStore(LAST_WINDOW).put(json, KEY);
      transaction.oncomplete = () => resolve();
      transaction.onabort = () => reject(transaction.error ?? new Error('Window write aborted'));
      transaction.onerror = () => reject(transaction.error ?? new Error('Window write failed'));
    });
  } finally {
    db.close();
  }
}

export function saveLastWindow(json) {
  pendingWindowJson = json;
  if (!activeWindowWrite) {
    activeWindowWrite = (async () => {
      try {
        while (pendingWindowJson !== undefined) {
          const next = pendingWindowJson;
          pendingWindowJson = undefined;
          await writeLastWindow(next);
        }
      } finally {
        activeWindowWrite = undefined;
      }
    })();
  }
  return activeWindowWrite;
}

// A settings change commits one complete snapshot. Serialize writes within
// this tab so a slower earlier transaction cannot overwrite a newer choice.
let settingsWrite = Promise.resolve();

export async function loadSettings() {
  await settingsWrite.catch(() => {});
  const db = await openDatabase();
  try {
    return await new Promise((resolve, reject) => {
      const transaction = db.transaction(BROWSER_SETTINGS, 'readonly');
      const request = transaction.objectStore(BROWSER_SETTINGS).get(KEY);
      let json = null;
      request.onsuccess = () => { json = request.result ?? null; };
      transaction.oncomplete = () => resolve(json);
      transaction.onabort = () => reject(transaction.error ?? new Error('Settings read aborted'));
      transaction.onerror = () => reject(transaction.error ?? new Error('Settings read failed'));
    });
  } finally {
    db.close();
  }
}

export function saveSettings(json) {
  settingsWrite = settingsWrite.catch(() => {}).then(async () => {
    const db = await openDatabase();
    try {
      await new Promise((resolve, reject) => {
        const transaction = db.transaction(BROWSER_SETTINGS, 'readwrite');
        transaction.objectStore(BROWSER_SETTINGS).put(json, KEY);
        transaction.oncomplete = () => resolve();
        transaction.onabort = () => reject(transaction.error ?? new Error('Settings write aborted'));
        transaction.onerror = () => reject(transaction.error ?? new Error('Settings write failed'));
      });
    } finally {
      db.close();
    }
  });
  return settingsWrite;
}

export async function loadCatalog() {
  const db = await openDatabase();
  try {
    return await new Promise((resolve, reject) => {
      const transaction = db.transaction(STORE, "readonly");
      const request = transaction.objectStore(STORE).get(KEY);
      request.onsuccess = () => {
        try {
          resolve(JSON.stringify(catalogFromRecord(request.result)));
        } catch (error) {
          reject(error);
        }
      };
      request.onerror = () => reject(request.error ?? new Error("Could not read catalog"));
      transaction.onabort = () => reject(transaction.error ?? new Error("Catalog read aborted"));
    });
  } finally {
    db.close();
  }
}

export async function saveCatalog(expected, json, secretJson = 'null') {
  const db = await openDatabase();
  try {
    return await new Promise((resolve, reject) => {
      const transaction = db.transaction([STORE, CREDENTIALS], "readwrite");
      const store = transaction.objectStore(STORE);
      const credentials = transaction.objectStore(CREDENTIALS);
      const request = store.get(KEY);
      let matched = false;
      request.onsuccess = () => {
        let current;
        try {
          current = catalogFromRecord(request.result);
        } catch (error) {
          transaction.abort();
          reject(error);
          return;
        }
        matched = String(current.generation) === expected;
        if (matched) {
          let catalog;
          let secret;
          try {
            catalog = JSON.parse(json);
            secret = JSON.parse(secretJson);
          } catch (error) {
            transaction.abort();
            reject(error);
            return;
          }
          if (catalog.generation !== current.generation + 1) {
            transaction.abort();
            reject(new Error('Connection catalog generation did not advance once'));
            return;
          }
          const retained = new Set(catalog.servers.flatMap((server) =>
            (server.profiles ?? []).map((profile) => credentialKey(server.name, profile.name))));
          for (const server of current.servers) {
            for (const profile of server.profiles ?? []) {
              const key = credentialKey(server.name, profile.name);
              if (!retained.has(key)) credentials.delete(key);
            }
          }
          if (secret) {
            const key = credentialKey(secret.server, secret.profile);
            if (!retained.has(key) || (secret.value !== null && typeof secret.value !== 'string')) {
              transaction.abort();
              reject(new Error('Credential change does not belong to a saved profile'));
              return;
            }
            if (secret.value === null) credentials.delete(key);
            else credentials.put(secret.value, key);
          }
          store.put(JSON.stringify({ schema: CATALOG_SCHEMA, catalog }), KEY);
        }
      };
      transaction.oncomplete = () => resolve(matched);
      transaction.onabort = () => reject(transaction.error ?? new Error("Catalog write aborted"));
      transaction.onerror = () => reject(transaction.error ?? new Error("Catalog write failed"));
    });
  } finally {
    db.close();
  }
}

export async function loadProfilePassword(server, profile) {
  const db = await openDatabase();
  try {
    return await new Promise((resolve, reject) => {
      const transaction = db.transaction(CREDENTIALS, 'readonly');
      const request = transaction.objectStore(CREDENTIALS).get(credentialKey(server, profile));
      request.onsuccess = () => resolve(request.result ?? null);
      request.onerror = () => reject(request.error ?? new Error('Could not read saved password'));
      transaction.onabort = () => reject(transaction.error ?? new Error('Password read aborted'));
    });
  } finally {
    db.close();
  }
}

// Layout identity is folded by Rust's portable naming policy before it
// reaches this adapter. IndexedDB array keys keep servers isolated without
// depending on a delimiter that a user could put in a server name.
const layoutKey = (server, foldedName) => [server, foldedName];

export async function listLayouts(server) {
  const db = await openDatabase();
  try {
    return await new Promise((resolve, reject) => {
      const transaction = db.transaction(LAYOUTS, 'readonly');
      const request = transaction.objectStore(LAYOUTS).index('server').getAll(server);
      let records;
      request.onsuccess = () => { records = request.result; };
      transaction.oncomplete = () => resolve(JSON.stringify(records.map((record) => record.name)));
      transaction.onabort = () => reject(transaction.error ?? new Error('Layout list aborted'));
      transaction.onerror = () => reject(transaction.error ?? new Error('Layout list failed'));
    });
  } finally {
    db.close();
  }
}

export async function layoutExists(server, foldedName) {
  const db = await openDatabase();
  try {
    return await new Promise((resolve, reject) => {
      const transaction = db.transaction(LAYOUTS, 'readonly');
      const request = transaction.objectStore(LAYOUTS).getKey(layoutKey(server, foldedName));
      let exists = false;
      request.onsuccess = () => { exists = request.result !== undefined; };
      transaction.oncomplete = () => resolve(exists);
      transaction.onabort = () => reject(transaction.error ?? new Error('Layout check aborted'));
      transaction.onerror = () => reject(transaction.error ?? new Error('Layout check failed'));
    });
  } finally {
    db.close();
  }
}

export async function loadLayout(server, foldedName) {
  const db = await openDatabase();
  try {
    return await new Promise((resolve, reject) => {
      const transaction = db.transaction(LAYOUTS, 'readonly');
      const request = transaction.objectStore(LAYOUTS).get(layoutKey(server, foldedName));
      let record;
      request.onsuccess = () => { record = request.result; };
      transaction.oncomplete = () => resolve(record?.json ?? null);
      transaction.onabort = () => reject(transaction.error ?? new Error('Layout read aborted'));
      transaction.onerror = () => reject(transaction.error ?? new Error('Layout read failed'));
    });
  } finally {
    db.close();
  }
}

export async function saveLayout(server, foldedName, name, json, replace = false) {
  const db = await openDatabase();
  try {
    return await new Promise((resolve, reject) => {
      const transaction = db.transaction(LAYOUTS, 'readwrite');
      const store = transaction.objectStore(LAYOUTS);
      const request = store.get(layoutKey(server, foldedName));
      request.onsuccess = () => {
        if (request.result && !replace) {
          transaction.abort();
          reject(new Error('Layout already exists; confirm overwrite'));
          return;
        }
        // Match native save semantics: overwriting a case variant keeps the
        // first saved display stem. Rename, by contrast, updates casing.
        store.put({ server, name: request.result?.name ?? name, json },
          layoutKey(server, foldedName));
      };
      transaction.oncomplete = () => resolve();
      transaction.onabort = () => reject(transaction.error ?? new Error('Layout save aborted'));
      transaction.onerror = () => reject(transaction.error ?? new Error('Layout save failed'));
    });
  } finally {
    db.close();
  }
}

export async function renameLayout(server, fromFolded, toFolded, toName, replace = false) {
  const db = await openDatabase();
  try {
    return await new Promise((resolve, reject) => {
      const transaction = db.transaction(LAYOUTS, 'readwrite');
      const store = transaction.objectStore(LAYOUTS);
      const from = layoutKey(server, fromFolded);
      const to = layoutKey(server, toFolded);
      const request = store.get(from);
      request.onsuccess = () => {
        if (!request.result) {
          transaction.abort();
          reject(new Error('Layout to rename no longer exists'));
          return;
        }
        if (fromFolded === toFolded) {
          store.put({ ...request.result, name: toName }, to);
          return;
        }
        const target = store.get(to);
        target.onsuccess = () => {
          if (target.result && !replace) {
            transaction.abort();
            reject(new Error('Destination layout already exists; confirm replacement'));
            return;
          }
          store.delete(from);
          store.put({ ...request.result, name: toName }, to);
        };
      };
      transaction.oncomplete = () => resolve();
      transaction.onabort = () => reject(transaction.error ?? new Error('Layout rename aborted'));
      transaction.onerror = () => reject(transaction.error ?? new Error('Layout rename failed'));
    });
  } finally {
    db.close();
  }
}

export async function deleteLayout(server, foldedName) {
  const db = await openDatabase();
  try {
    return await new Promise((resolve, reject) => {
      const transaction = db.transaction(LAYOUTS, 'readwrite');
      transaction.objectStore(LAYOUTS).delete(layoutKey(server, foldedName));
      transaction.oncomplete = () => resolve();
      transaction.onabort = () => reject(transaction.error ?? new Error('Layout delete aborted'));
      transaction.onerror = () => reject(transaction.error ?? new Error('Layout delete failed'));
    });
  } finally {
    db.close();
  }
}
