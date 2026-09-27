// Hermetic browser fixture: release assets over HTTP and a genuine local WSS
// MUD. Certificates are generated in memory; no private key is checked in.
import { createHash } from 'node:crypto';
import { createReadStream, readFileSync } from 'node:fs';
import { createServer as createHttpServer } from 'node:http';
import { createServer as createHttpsServer } from 'node:https';
import { extname, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import selfsigned from 'selfsigned';
import { WebSocketServer } from 'ws';

const root = resolve(fileURLToPath(new URL('../e2e-dist/', import.meta.url)));
const pagePort = 8094;
const runtimePort = 8095;
const mudPort = 9443;
const sessions = new Map();

const types = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.wasm': 'application/wasm',
  '.css': 'text/css; charset=utf-8',
  '.svg': 'image/svg+xml',
};

function session(id) {
  if (!sessions.has(id)) {
    sessions.set(id, { opens: 0, closes: 0, sockets: new Set(), messages: [], origins: [], autoGreeting: true });
  }
  return sessions.get(id);
}

function sendJson(response, value, status = 200) {
  response.writeHead(status, { 'content-type': 'application/json' });
  response.end(JSON.stringify(value));
}

async function requestBody(request) {
  const chunks = [];
  for await (const chunk of request) {
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString('utf8'));
}

const index = readFileSync(resolve(root, 'index.html'), 'utf8').replace(
  'https://runtime.smudgy.org/runtime-frame.html',
  `http://127.0.0.1:${runtimePort}/runtime-frame.html`,
);
const runtimeFrame = readFileSync(resolve(root, 'runtime-frame.html'), 'utf8').replace(
  'https://web.smudgy.org', `http://127.0.0.1:${pagePort}`,
);
const workerPage = readFileSync(new URL('./worker.html', import.meta.url));
const hashes = [...index.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)].map(
  ([, source]) => `'sha256-${createHash('sha256').update(source).digest('base64')}'`,
);
const csp = [
  "default-src 'none'",
  `script-src 'self' blob: 'wasm-unsafe-eval' ${hashes.join(' ')}`,
  "worker-src 'self'",
  `frame-src http://127.0.0.1:${runtimePort}`,
  `connect-src 'self' wss://127.0.0.1:${mudPort}`,
  "style-src 'self' 'unsafe-inline'",
  "font-src 'self' data:",
  "img-src 'self' data: blob:",
].join('; ');
const runtimeCsp = [
  "default-src 'none'",
  "script-src 'self' blob: 'wasm-unsafe-eval'",
  "worker-src 'self'",
  `connect-src 'self' wss://127.0.0.1:${mudPort}`,
  `frame-ancestors http://127.0.0.1:${pagePort}`,
].join('; ');

const pageServer = createHttpServer(async (request, response) => {
  try {
    const url = new URL(request.url, `http://127.0.0.1:${pagePort}`);
    if (url.pathname === '/__health') {
      sendJson(response, { ready: true });
      return;
    }
    if (url.pathname === '/__test/reset' && request.method === 'POST') {
      for (const target of sessions.values()) {
        for (const socket of target.sockets) socket.terminate();
      }
      sessions.clear();
      sendJson(response, { reset: true });
      return;
    }
    if (url.pathname === '/__test/state') {
      const { opens, closes, sockets, messages, origins } = session(url.searchParams.get('id'));
      sendJson(response, { opens, closes, active: sockets.size, messages, origins });
      return;
    }
    if (url.pathname === '/__test/configure' && request.method === 'POST') {
      const { autoGreeting } = await requestBody(request);
      session(url.searchParams.get('id')).autoGreeting = autoGreeting;
      sendJson(response, { configured: true });
      return;
    }
    if (url.pathname === '/__test/emit' && request.method === 'POST') {
      const { base64, text } = await requestBody(request);
      const target = session(url.searchParams.get('id'));
      for (const socket of target.sockets) {
        if (typeof text === 'string') socket.send(text);
        else socket.send(Buffer.from(base64, 'base64'), { binary: true });
      }
      sendJson(response, { sent: target.sockets.size });
      return;
    }
    if (url.pathname === '/__test/drop' && request.method === 'POST') {
      const target = session(url.searchParams.get('id'));
      const count = target.sockets.size;
      for (const socket of target.sockets) socket.terminate();
      sendJson(response, { dropped: count });
      return;
    }
    if (url.pathname === '/e2e/worker.html') {
      response.writeHead(200, {
        'content-type': types['.html'],
        'content-security-policy': csp,
      });
      response.end(workerPage);
      return;
    }
    if (url.pathname === '/' || url.pathname === '/index.html') {
      response.writeHead(200, {
        'content-type': types['.html'],
        'content-security-policy': csp,
      });
      response.end(index);
      return;
    }
    const pathname = decodeURIComponent(url.pathname === '/' ? '/index.html' : url.pathname);
    const filename = resolve(root, `.${pathname}`);
    if (!filename.startsWith(root + sep)) {
      sendJson(response, { error: 'invalid path' }, 400);
      return;
    }
    response.setHeader('content-security-policy', csp);
    response.setHeader('content-type', types[extname(filename)] ?? 'application/octet-stream');
    createReadStream(filename).on('error', () => {
      if (!response.headersSent) sendJson(response, { error: 'not found' }, 404);
      else response.destroy();
    }).pipe(response);
  } catch (error) {
    sendJson(response, { error: String(error) }, 500);
  }
});

const runtimeAssets = new Set([
  '/runtime-frame.html',
  '/runtime-broker.js',
  '/session-worker.js',
  '/smudgy-web-worker.js',
  '/smudgy-web-worker_bg.wasm',
]);
const runtimeServer = createHttpServer((request, response) => {
  const url = new URL(request.url, `http://127.0.0.1:${runtimePort}`);
  if (!runtimeAssets.has(url.pathname)) {
    sendJson(response, { error: 'not found' }, 404);
    return;
  }
  if (url.pathname === '/runtime-frame.html') {
    response.writeHead(200, {
      'content-type': types['.html'],
      'content-security-policy': runtimeCsp,
    });
    response.end(runtimeFrame);
    return;
  }
  response.setHeader('content-security-policy', runtimeCsp);
  response.setHeader('content-type', types[extname(url.pathname)] ?? 'application/octet-stream');
  createReadStream(resolve(root, `.${url.pathname}`)).on('error', () => {
    if (!response.headersSent) sendJson(response, { error: 'not found' }, 404);
    else response.destroy();
  }).pipe(response);
});

export async function startFixture() {
  const cert = await selfsigned.generate([{ name: 'commonName', value: 'localhost' }], {
    algorithm: 'sha256',
    extensions: [
      { name: 'basicConstraints', cA: false },
      { name: 'keyUsage', digitalSignature: true, keyEncipherment: true },
      { name: 'subjectAltName', altNames: [
        { type: 2, value: 'localhost' },
        { type: 7, ip: '127.0.0.1' },
      ] },
    ],
  });
  const mudServer = createHttpsServer({ key: cert.private, cert: cert.cert });
  const sockets = new WebSocketServer({ noServer: true });

  mudServer.on('upgrade', (request, socket, head) => {
    const url = new URL(request.url, `https://127.0.0.1:${mudPort}`);
    const match = /^\/mud\/([a-z0-9-]+)$/.exec(url.pathname);
    if (!match) {
      socket.destroy();
      return;
    }
    sockets.handleUpgrade(request, socket, head, (websocket) => {
      const target = session(match[1]);
      target.opens += 1;
      target.origins.push(request.headers.origin ?? null);
      target.sockets.add(websocket);
      websocket.on('message', (data) => target.messages.push(Buffer.from(data).toString('base64')));
      websocket.on('close', () => {
        target.closes += 1;
        target.sockets.delete(websocket);
      });
      if (target.autoGreeting) {
        websocket.send(Buffer.from(`Welcome ${match[1]}\r\n`), { binary: true });
      }
    });
  });

  await new Promise((resolveListen) => mudServer.listen(mudPort, '127.0.0.1', resolveListen));
  await new Promise((resolveListen) => pageServer.listen(pagePort, '127.0.0.1', resolveListen));
  await new Promise((resolveListen) => runtimeServer.listen(runtimePort, '127.0.0.1', resolveListen));
  console.log(`Smudgy E2E fixture: http://127.0.0.1:${pagePort}, runtime :${runtimePort}, wss://127.0.0.1:${mudPort}`);
  return async () => {
    for (const target of sessions.values()) {
      for (const socket of target.sockets) socket.terminate();
    }
    await new Promise((resolveClose) => sockets.close(resolveClose));
    await new Promise((resolveClose) => mudServer.close(resolveClose));
    await new Promise((resolveClose) => pageServer.close(resolveClose));
    await new Promise((resolveClose) => runtimeServer.close(resolveClose));
  };
}
