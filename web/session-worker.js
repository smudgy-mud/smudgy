// Trunk emits stable, worker-only assets for data-type="worker". This classic
// worker loads that binding once, then serializes startup and later commands.
const pending = [];
let ready = false;
let draining = false;
let failed = false;

self.onmessage = (event) => {
  if (failed) {
    return;
  }
  pending.push(event.data);
  void drain();
};

function fatal(error) {
  failed = true;
  pending.length = 0;
  self.postMessage({ kind: "fatal", message: String(error) });
}

async function loadScript(message) {
  const plan = JSON.parse(message.automation);
  const scriptSources = [plan.shared?.script, plan.profile?.script]
    .filter((source) => typeof source === 'string' && source.trim());
  const scriptFactories = [];
  for (const script of scriptSources) {
    // Each scope compiles independently, so a profile can use the same local
    // names as Default without overwriting its handlers. Event callbacks stay
    // synchronous inside the owning session worker.
    const moduleSource = `export default function create(api) {
"use strict";
const paneCall = (value) => {
  if (value && typeof value === "object" && typeof value.error === "string") {
    throw new Error(value.error);
  }
  return value;
};
const pane = (name, created = undefined) => Object.freeze({
  name,
  isMain: name.toLowerCase() === "main",
  created,
  echo(text) { paneCall(api.__paneEcho(name, String(text))); },
  clear() { paneCall(api.__paneClear(name)); },
  close() { paneCall(api.__paneClose(name)); },
  split(direction, spec) {
    const result = paneCall(api.__paneSplit(name, direction, spec));
    return pane(result.name, result.created);
  },
  addTab(spec) {
    const result = paneCall(api.__paneAddTab(name, spec));
    return pane(result.name, result.created);
  },
  hide() { paneCall(api.__paneSetHidden(name, true)); },
  show() { paneCall(api.__paneSetHidden(name, false)); },
  get isHidden() { return Boolean(api.__paneHidden(name)); },
});
api.session = Object.freeze({ mainPane: pane("main") });
${script}
return {
  onLine: typeof onLine === "function" ? onLine : null,
  onInput: typeof onInput === "function" ? onInput : null,
};
}`;
    const url = URL.createObjectURL(new Blob([moduleSource], { type: 'text/javascript' }));
    try {
      const module = await import(url);
      scriptFactories.push(module.default);
    } finally {
      URL.revokeObjectURL(url);
    }
  }

  const packageNames = JSON.parse(message.packages || '[]');
  if (!Array.isArray(packageNames) || packageNames.some((name) => typeof name !== 'string')) {
    throw new Error('Session has an invalid installed-package list');
  }
  const registries = await loadPackages(packageNames, message.package_sources);
  if (scriptFactories.length === 0 && registries.length === 0) return;
  message.script_factory = (api) => {
    const handlers = scriptFactories.map((factory) => factory(api));
    for (const registry of registries) registry.api = api;
    return {
      onLine(line) {
        for (const scope of handlers) {
          const result = scope?.onLine?.(line, api);
          if (result instanceof Promise) throw new Error('Session script onLine must be synchronous');
        }
        for (const registry of registries) runMatching(registry.triggers, line, false);
      },
      onInput(input) {
        for (let index = handlers.length - 1; index >= 0; index -= 1) {
          const result = handlers[index]?.onInput?.(input, api);
          if (result instanceof Promise) throw new Error('Session script onInput must be synchronous');
          if (result === true) return true;
        }
        for (const registry of registries) {
          if (runMatching(registry.aliases, input, true)) return true;
        }
        return false;
      },
    };
  };
}

function runMatching(handlers, text, firstOnly) {
  let matched = false;
  for (const entry of handlers) {
    entry.pattern.lastIndex = 0;
    const match = entry.pattern.exec(text);
    if (!match) continue;
    const result = entry.callback(match.groups || match);
    if (result instanceof Promise) throw new Error('Package callbacks must be synchronous');
    matched = true;
    if (firstOnly) break;
  }
  return matched;
}

function packageRegistry() {
  const registry = {
    api: null,
    aliases: [],
    triggers: [],
    send(command) {
      if (!registry.api) throw new Error('send is unavailable during package initialization');
      registry.api.send(command);
    },
    echo(line) {
      if (!registry.api) throw new Error('echo is unavailable during package initialization');
      registry.api.print(line);
    },
    register(kind, pattern, callback) {
      if (typeof callback !== 'function') throw new Error(`${kind} callback must be a function`);
      if (!(pattern instanceof RegExp) && typeof pattern !== 'string') {
        throw new Error(`${kind} pattern must be a string or RegExp`);
      }
      const entry = { pattern: pattern instanceof RegExp ? pattern : new RegExp(pattern), callback };
      const list = kind === 'alias' ? registry.aliases : registry.triggers;
      list.push(entry);
      return { delete() {
        const index = list.indexOf(entry);
        if (index !== -1) list.splice(index, 1);
      } };
    },
  };
  return registry;
}

async function loadPackages(names, sources) {
  if (names.length === 0) return [];
  if (!Array.isArray(sources) || sources.length !== names.length) {
    throw new Error('Session has no compiled source for every installed package');
  }
  const registries = [];
  for (let index = 0; index < names.length; index += 1) {
    const source = sources[index];
    if (source?.name !== names[index] || typeof source.code !== 'string') {
      throw new Error(`Session has invalid compiled source for package ${names[index]}`);
    }
    const registry = packageRegistry();
    globalThis.__smudgyPackageRegistry = registry;
    const url = URL.createObjectURL(new Blob([source.code], { type: 'text/javascript' }));
    try {
      await import(url);
      registries.push(registry);
    } finally {
      delete globalThis.__smudgyPackageRegistry;
      URL.revokeObjectURL(url);
    }
  }
  return registries;
}

async function drain() {
  if (!ready || draining) {
    return;
  }
  draining = true;
  try {
    while (pending.length !== 0) {
      const message = pending.shift();
      if (message.kind === "start") {
        await loadScript(message);
      }
      wasm_bindgen.worker_message(message);
    }
  } catch (error) {
    fatal(error);
  } finally {
    draining = false;
  }
}

(async () => {
  importScripts("smudgy-web-worker.js");
  await wasm_bindgen({
    module_or_path: new URL("smudgy-web-worker_bg.wasm", self.location.href),
  });
  ready = true;
  await drain();
})().catch((error) => {
  fatal(error);
});
