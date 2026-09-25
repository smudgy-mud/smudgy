// This frame is served from the script origin. It has no catalog, credentials,
// or auth cookies. Its sole job is to bind a channel to one session worker.
const parentOrigin = new URL(location.href).searchParams.get('parent_origin');
const allowedOrigin = document.querySelector('meta[name="smudgy-web-origin"]')?.content;
if (!parentOrigin || parentOrigin !== allowedOrigin || new URL(parentOrigin).origin !== parentOrigin) {
  throw new Error('Script runtime has no valid parent origin');
}

window.addEventListener('message', (event) => {
  if (event.source !== window.parent || event.origin !== parentOrigin) return;
  if (event.data?.kind === 'smudgy-runtime-probe-v1') {
    window.parent.postMessage({ kind: 'smudgy-runtime-ready-v1' }, parentOrigin);
    return;
  }
  if (event.data?.kind !== 'smudgy-runtime-open-v1' || event.ports.length !== 1) return;
  const port = event.ports[0];
  let worker;
  try {
    worker = new Worker('session-worker.js');
  } catch (error) {
    port.postMessage({ kind: 'fatal', message: String(error) });
    port.close();
    return;
  }
  worker.onmessage = ({ data }) => {
    if (data instanceof ArrayBuffer) port.postMessage(data, [data]);
    else port.postMessage(data);
  };
  worker.onerror = (error) => {
    port.postMessage({ kind: 'fatal', message: error.message || 'Script worker failed' });
  };
  port.onmessage = ({ data }) => {
    if (data?.kind === 'smudgy-runtime-terminate-v1') {
      worker.terminate();
      port.close();
    } else {
      worker.postMessage(data);
    }
  };
  port.start();
});

window.parent.postMessage({ kind: 'smudgy-runtime-ready-v1' }, parentOrigin);
