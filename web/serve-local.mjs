// Serve one Trunk build as two separate loopback origins for manual testing.
import { createHash } from 'node:crypto';
import { createReadStream, readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { extname, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('./dist/', import.meta.url)));
const webPort = Number(process.env.SMUDGY_WEB_PORT ?? 8096);
const scriptPort = Number(process.env.SMUDGY_SCRIPT_PORT ?? 8097);
if (![webPort, scriptPort].every((port) => Number.isInteger(port) && port > 0 && port <= 65535)
    || webPort === scriptPort) {
  throw new Error('SMUDGY_WEB_PORT and SMUDGY_SCRIPT_PORT must be distinct valid ports');
}
const webOrigin = `http://127.0.0.1:${webPort}`;
const scriptOrigin = `http://127.0.0.1:${scriptPort}`;
const index = readFileSync(resolve(root, 'index.html'), 'utf8').replace(
  'https://runtime.smudgy.org/runtime-frame.html',
  `${scriptOrigin}/runtime-frame.html`,
);
const runtimeFrame = readFileSync(resolve(root, 'runtime-frame.html'), 'utf8').replace(
  'https://web.smudgy.org', webOrigin,
);
const inlineHashes = [...index.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)].map(
  ([, source]) => `'sha256-${createHash('sha256').update(source).digest('base64')}'`,
);
const webCsp = [
  "default-src 'none'",
  `script-src 'self' blob: 'wasm-unsafe-eval' ${inlineHashes.join(' ')}`,
  "worker-src 'self'",
  `frame-src ${scriptOrigin}`,
  "connect-src 'self'",
  "style-src 'self' 'unsafe-inline'",
  "font-src 'self' data:",
  "img-src 'self' data: blob:",
].join('; ');
const scriptCsp = [
  "default-src 'none'",
  "script-src 'self' blob: 'wasm-unsafe-eval'",
  "worker-src 'self'",
  "connect-src 'self' wss:",
  `frame-ancestors ${webOrigin}`,
].join('; ');
const runtimeAssets = new Set([
  '/runtime-frame.html', '/runtime-broker.js', '/session-worker.js',
  '/smudgy-web-worker.js', '/smudgy-web-worker_bg.wasm',
]);
const types = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.wasm': 'application/wasm',
  '.css': 'text/css; charset=utf-8',
  '.svg': 'image/svg+xml',
};

function serve(request, response, runtime) {
  const origin = runtime ? scriptOrigin : webOrigin;
  let pathname;
  try {
    const url = new URL(request.url, origin);
    pathname = decodeURIComponent(url.pathname === '/' ? '/index.html' : url.pathname);
  } catch {
    response.writeHead(400).end();
    return;
  }
  if (runtime && !runtimeAssets.has(pathname)) {
    response.writeHead(404).end();
    return;
  }
  if (runtime && pathname === '/runtime-frame.html') {
    response.writeHead(200, {
      'content-type': types['.html'],
      'content-security-policy': scriptCsp,
      'cache-control': 'no-store',
    });
    response.end(runtimeFrame);
    return;
  }
  if (!runtime && pathname === '/index.html') {
    response.writeHead(200, {
      'content-type': types['.html'],
      'content-security-policy': webCsp,
      'cache-control': 'no-store',
    });
    response.end(index);
    return;
  }
  const filename = resolve(root, `.${pathname}`);
  if (!filename.startsWith(root + sep)) {
    response.writeHead(400).end();
    return;
  }
  response.setHeader('content-security-policy', runtime ? scriptCsp : webCsp);
  response.setHeader('content-type', types[extname(filename)] ?? 'application/octet-stream');
  response.setHeader('cache-control', 'no-store');
  createReadStream(filename).on('error', () => {
    if (!response.headersSent) response.writeHead(404).end();
    else response.destroy();
  }).pipe(response);
}

createServer((request, response) => serve(request, response, false)).listen(webPort, '127.0.0.1');
createServer((request, response) => serve(request, response, true)).listen(scriptPort, '127.0.0.1');
console.log(`Smudgy web: ${webOrigin}/; script runtime: ${scriptOrigin}/runtime-frame.html`);
