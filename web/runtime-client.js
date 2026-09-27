// The window owns persistent data. Script workers only receive the selected
// session's compiled packages over a channel to the separate runtime origin.
const runtimeUrl = document.querySelector('meta[name="smudgy-script-runtime"]')?.content;
if (!runtimeUrl) throw new Error('Smudgy script runtime URL is not configured');
const runtime = new URL(runtimeUrl);
const loopbackHttp = runtime.protocol === 'http:'
  && (runtime.hostname === '127.0.0.1' || runtime.hostname === 'localhost');
if (runtime.protocol !== 'https:' && !loopbackHttp) {
  throw new Error('Smudgy script runtime must use HTTPS');
}
if (runtime.origin === location.origin) {
  throw new Error('Smudgy script runtime must have a separate origin');
}

let brokerPromise;
let brokerAttempt = 0;
let reportedBrokerFailure = 0;

class BrokerUnavailableError extends Error {
  constructor(message, attempt) {
    super(message);
    this.attempt = attempt;
  }
}

function broker() {
  brokerPromise ??= new Promise((resolve, reject) => {
    const attempt = ++brokerAttempt;
    const frame = document.createElement('iframe');
    frame.hidden = true;
    frame.setAttribute('aria-hidden', 'true');
    frame.setAttribute('sandbox', 'allow-scripts allow-same-origin');
    const timeout = setTimeout(() => finish(new BrokerUnavailableError(
      'Script runtime did not become ready', attempt,
    )), 10000);
    let retry;
    let navigation = 0;
    let settled = false;
    function finish(error) {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      clearTimeout(retry);
      window.removeEventListener('message', receive);
      frame.onload = null;
      frame.onerror = null;
      if (error) {
        frame.remove();
        reject(error);
      } else {
        resolve(frame.contentWindow);
      }
    }
    function receive(event) {
      if (event.source !== frame.contentWindow || event.origin !== runtime.origin) return;
      if (event.data?.kind !== 'smudgy-runtime-ready-v1') return;
      finish();
    }
    function probe() {
      frame.contentWindow?.postMessage({ kind: 'smudgy-runtime-probe-v1' }, runtime.origin);
    }
    function load() {
      if (settled) return;
      clearTimeout(retry);
      const url = new URL(runtime);
      url.searchParams.set('parent_origin', location.origin);
      url.searchParams.set('broker_attempt', `${attempt}-${++navigation}`);
      frame.src = url.href;
    }
    function loaded() {
      probe();
      // Chromium reports a failed iframe navigation as a load rather than an
      // error. Reload after a short grace period so an origin that is coming
      // online can recover within this broker attempt.
      retry = setTimeout(load, 1000);
    }
    window.addEventListener('message', receive);
    frame.onload = loaded;
    frame.onerror = () => { retry = setTimeout(load, 1000); };
    load();
    document.body.append(frame);
  }).catch((error) => {
    // A temporary script-origin outage must not poison every later session.
    brokerPromise = undefined;
    throw error;
  });
  return brokerPromise;
}

async function packageSources(names) {
  if (!Array.isArray(names) || names.some((name) => typeof name !== 'string')) {
    throw new Error('Session has an invalid installed-package list');
  }
  if (names.length === 0) return [];
  const database = await new Promise((resolve, reject) => {
    const request = indexedDB.open('smudgy-web-packages', 1);
    request.onupgradeneeded = () => request.result.createObjectStore('packages', { keyPath: 'name' });
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  try {
    const sources = [];
    for (const name of names) {
      const record = await new Promise((resolve, reject) => {
        const request = database.transaction('packages').objectStore('packages').get(name);
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
      });
      if (!record || typeof record.code !== 'string') {
        throw new Error(`Local package ${name} is not installed in this browser`);
      }
      sources.push({ name, code: record.code });
    }
    return sources;
  } finally {
    database.close();
  }
}

export class SessionProxy {
  constructor(onMessage) {
    this.onMessage = onMessage;
    this.closed = false;
    this.port = null;
    this.frame = broker();
    this.queue = Promise.resolve();
  }

  async connect() {
    if (this.port) return;
    const frame = await this.frame;
    if (this.closed) return;
    const channel = new MessageChannel();
    this.port = channel.port1;
    this.port.onmessage = this.onMessage;
    this.port.onmessageerror = () => this.fail(new Error('Script runtime sent an invalid message'));
    this.port.start();
    frame.postMessage({ kind: 'smudgy-runtime-open-v1' }, runtime.origin, [channel.port2]);
  }

  postMessage(message) {
    this.queue = this.queue.then(async () => {
      if (this.closed) return;
      if (message.kind === 'start') {
        const names = JSON.parse(message.packages || '[]');
        message = { ...message, package_sources: await packageSources(names) };
      }
      await this.connect();
      if (!this.closed) this.port.postMessage(message);
    }).catch((error) => this.fail(error));
  }

  terminate() {
    this.closed = true;
    if (this.port) {
      this.port.postMessage({ kind: 'smudgy-runtime-terminate-v1' });
      this.port.close();
      this.port = null;
    }
  }

  fail(error) {
    if (this.closed) return;
    let kind = 'fatal';
    if (error instanceof BrokerUnavailableError) {
      if (reportedBrokerFailure === error.attempt) {
        kind = 'runtime-unavailable';
      } else {
        reportedBrokerFailure = error.attempt;
        kind = 'runtime-fatal';
      }
    }
    this.onMessage(new MessageEvent('message', {
      data: { kind, message: String(error) },
    }));
  }
}

export function createSessionWorker(onMessage) {
  return new SessionProxy(onMessage);
}
