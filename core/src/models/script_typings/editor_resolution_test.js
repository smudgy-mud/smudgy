// Runs only in a Rust test, with the vendored TypeScript compiler and the actual
// generated tsconfig. No filesystem, network, Node installation, or editor plugin.
(() => {
  const source = `
import { value as smudgyValue } from '@tools/feature';
import { value as npmValue } from 'npm:@tools/feature';
import { value as rootValue } from '@tools';
import { value as localValue } from '@local';
import { smudgy, npm, other as ordinaryValue } from 'npm:consumer';
const a: 'smudgy' = smudgyValue;
const b: 'npm' = npmValue;
const c: 'root' = rootValue;
const d: 'local' = localValue;
const e: 'other' = ordinaryValue;
const f: 'smudgy' = smudgy;
const g: 'npm' = npm;
`;
  const files = new Map(Object.entries({
    '/server/.smudgy/tsconfig.base.json': managedConfig,
    '/server/tsconfig.json': '{"extends":"./.smudgy/tsconfig.base.json","files":["modules/main.ts"]}',
    '/server/modules/main.ts': source,
    '/server/.smudgy/packages/author/tools/index.d.ts': 'export declare const value: "root";',
    '/server/.smudgy/packages/author/tools/feature.d.ts': 'export declare const value: "smudgy";',
    '/server/packages/local/index.ts': 'export const value = "local";',
    '/server/.smudgy/npm/tools/types.d.ts': 'export declare const value: "npm";',
    '/server/.smudgy/npm/consumer/node_modules/@other/pkg/index.d.ts': 'export declare const value: "other";',
    '/server/.smudgy/npm/consumer/index.d.ts': `
export { value as smudgy } from '@tools/feature';
export { value as npm } from 'npm:@tools/feature';
export { value as other } from '@other/pkg';
`,
  }));
  const fs = {
    useCaseSensitiveFileNames: true,
    fileExists: path => files.has(path),
    readFile: path => files.get(path),
    readDirectory: () => [],
    directoryExists: path => [...files.keys()].some(file => file.startsWith(path.replace(/\/$/, '') + '/')),
  };
  const config = ts.readConfigFile('/server/tsconfig.json', fs.readFile);
  const parsed = ts.parseJsonConfigFileContent(config.config, fs, '/server');
  if (config.error || parsed.errors.length) throw new Error('Invalid generated tsconfig');
  const options = { ...parsed.options, noLib: true, types: [] };
  const resolve = (name, from = '/server/modules/main.ts') =>
    ts.resolveModuleName(name, from, options, fs).resolvedModule?.resolvedFileName;
  const expect = (actual, expected) => {
    if (actual !== expected) throw new Error(`${actual} !== ${expected}`);
  };
  expect(resolve('@tools'), '/server/.smudgy/packages/author/tools/index.d.ts');
  expect(resolve('@tools/feature'), resolve('smudgy:@tools/feature'));
  expect(resolve('@tools/feature'), resolve('smudgy://author/tools/feature'));
  expect(resolve('@local'), '/server/packages/local/index.ts');
  expect(resolve('@tools/feature', '/server/.smudgy/npm/consumer/index.d.ts'), resolve('@tools/feature'));
  expect(resolve('npm:@tools/feature'), '/server/.smudgy/npm/tools/types.d.ts');
  expect(resolve('npm:not-imported-yet'), undefined);
  const service = ts.createLanguageService({
    ...fs,
    useCaseSensitiveFileNames: () => true,
    getCompilationSettings: () => options,
    getScriptFileNames: () => parsed.fileNames,
    getScriptVersion: () => '1',
    getScriptSnapshot: path => files.has(path) ? ts.ScriptSnapshot.fromString(files.get(path)) : undefined,
    getCurrentDirectory: () => '/server',
    getDefaultLibFileName: () => '/lib.d.ts',
  });
  const diagnostics = service.getSemanticDiagnostics('/server/modules/main.ts');
  if (diagnostics.length) {
    throw new Error(diagnostics.map(d => ts.flattenDiagnosticMessageText(d.messageText, '\n')).join('\n'));
  }
  for (const [name, type] of [['smudgyValue', 'smudgy'], ['npmValue', 'npm'], ['rootValue', 'root'], ['localValue', 'local']]) {
    const info = service.getQuickInfoAtPosition('/server/modules/main.ts', source.indexOf(name));
    const hint = ts.displayPartsToString(info?.displayParts);
    if (!hint.includes(`"${type}"`)) throw new Error(`Incorrect hover for ${name}: ${hint}`);
  }
  service.dispose();
})();
