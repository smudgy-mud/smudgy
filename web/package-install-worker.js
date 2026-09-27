// Installation is deliberately off the UI thread. Each imported package is
// compiled once; a session worker only loads its already-bundled JavaScript.
import * as esbuild from './browser.min.js';

const DATABASE = 'smudgy-web-packages';
const STORE = 'packages';
const MAX_SOURCE_BYTES = 16 * 1024 * 1024;
let compiler;

function openDatabase() {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE, { keyPath: 'name' });
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result);
  });
}

async function savePackage(record) {
  const database = await openDatabase();
  try {
    await new Promise((resolve, reject) => {
      const transaction = database.transaction(STORE, 'readwrite');
      const store = transaction.objectStore(STORE);
      const existing = store.get(record.name);
      existing.onsuccess = () => {
        if (existing.result) {
          if (existing.result.code !== record.code ||
              JSON.stringify(existing.result.manifest) !== JSON.stringify(record.manifest)) {
            reject(new Error(`Local package ${record.name} is already installed with different contents`));
            transaction.abort();
          }
        } else {
          store.put(record);
        }
      };
      transaction.oncomplete = resolve;
      transaction.onerror = () => reject(transaction.error);
      transaction.onabort = () => reject(transaction.error);
    });
  } finally {
    database.close();
  }
}

function normalize(path) {
  const segments = path.replaceAll('\\', '/').split('/');
  const result = [];
  for (const segment of segments) {
    if (segment === '' || segment === '.') continue;
    if (segment === '..') {
      if (result.length === 0) throw new Error('Package import escapes its directory');
      result.pop();
    } else {
      result.push(segment);
    }
  }
  return result.join('/');
}

function resolveFile(files, path) {
  for (const candidate of [path, `${path}.ts`, `${path}.tsx`, `${path}.js`, `${path}.jsx`, `${path}.json`, `${path}/index.ts`, `${path}/index.js`]) {
    if (files.has(candidate)) return candidate;
  }
  throw new Error(`Package module not found: ${path}`);
}

function parseManifest(files) {
  const source = files.get('smudgy.package.json');
  if (!source) throw new Error('Package has no smudgy.package.json');
  const manifest = JSON.parse(source);
  if (manifest.target !== 'web' && manifest.target !== 'both') {
    throw new Error('Package must declare target "web" or "both"');
  }
  if (manifest.min_smudgy_version) {
    throw new Error('The browser installer cannot verify min_smudgy_version yet');
  }
  if (manifest.requires?.length || manifest.params?.length) {
    throw new Error('Required packages and package parameters are not supported by the browser installer yet');
  }
  if (manifest.dependencies?.some((dependency) => !dependency.startsWith('.'))) {
    throw new Error('Only relative package dependencies are supported by the browser installer');
  }
  if (manifest.permissions && Object.keys(manifest.permissions).length !== 0) {
    throw new Error('This browser installer cannot grant native package permissions');
  }
  const entry = manifest.entry || ['index.ts', 'index.tsx', 'index.js', 'index.jsx', 'mod.ts', 'mod.js']
    .find((candidate) => files.has(candidate));
  if (!entry) throw new Error('Package has no entry module');
  return { manifest, entry: resolveFile(files, normalize(entry)) };
}

const coreShim = `
const host = globalThis.__smudgyPackageRegistry;
export const send = (command) => host.send(command);
export const echo = (line) => host.echo(line);
export const createAlias = (pattern, callback, options) => host.register('alias', pattern, callback, options);
export const createTrigger = (pattern, callback, options) => host.register('trigger', pattern, callback, options);
`;

async function compile(files, entry) {
  compiler ||= esbuild.initialize({ wasmURL: new URL('./esbuild.wasm', import.meta.url).href, worker: false });
  await compiler;
  const result = await esbuild.build({
    entryPoints: [entry],
    bundle: true,
    platform: 'browser',
    format: 'esm',
    target: 'es2022',
    write: false,
    plugins: [{
      name: 'local-smudgy-package',
      setup(build) {
        build.onResolve({ filter: /.*/ }, (args) => {
          if (args.kind === 'dynamic-import') {
            return { errors: [{ text: 'Dynamic imports are not supported in synchronous browser packages' }] };
          }
          if (args.path === 'smudgy:core') return { path: args.path, namespace: 'smudgy-core' };
          if (args.kind !== 'entry-point' && !args.path.startsWith('.')) {
            return { errors: [{ text: `Unsupported browser package import: ${args.path}` }] };
          }
          try {
            const base = args.importer ? args.importer.split('/').slice(0, -1).join('/') : '';
            const path = normalize(base ? `${base}/${args.path}` : args.path);
            return { path: resolveFile(files, path), namespace: 'package' };
          } catch (error) {
            return { errors: [{ text: String(error) }] };
          }
        });
        build.onLoad({ filter: /.*/, namespace: 'smudgy-core' }, () => ({ contents: coreShim, loader: 'js' }));
        build.onLoad({ filter: /.*/, namespace: 'package' }, (args) => ({
          contents: files.get(args.path),
          loader: args.path.endsWith('.tsx') ? 'tsx' : args.path.endsWith('.ts') ? 'ts'
            : args.path.endsWith('.jsx') ? 'jsx' : args.path.endsWith('.json') ? 'json' : 'js',
        }));
      },
    }],
  });
  if (result.outputFiles.length !== 1) throw new Error('Package compilation produced unexpected output');
  return result.outputFiles[0].text;
}

self.onmessage = async ({ data }) => {
  try {
    const files = new Map(data.files);
    const bytes = [...files.values()].reduce((total, source) => total + source.length * 2, 0);
    if (bytes > MAX_SOURCE_BYTES) throw new Error('Package source exceeds the 16 MiB import limit');
    if (!/^[A-Za-z0-9_-]+$/.test(data.name)) throw new Error('Package folder name is invalid');
    const { manifest, entry } = parseManifest(files);
    const code = await compile(files, entry);
    await savePackage({ name: data.name, manifest, code });
    self.postMessage({ name: data.name });
  } catch (error) {
    self.postMessage({ error: String(error) });
  }
};
