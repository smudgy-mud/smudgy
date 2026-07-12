// In-memory TypeScript declaration emit. Reads `globalThis.ts` (the loaded compiler)
// and `globalThis.__SMUDGY_DTS_INPUT` ({ libs, sources, rootNames, defaultLib }), and
// returns a JSON string `{ files, diagnostics }`. Runs inside smudgy's bare deno_core
// runtime at publish time — no filesystem, no npm, no network.
(function () {
  const ts = globalThis.ts;
  const input = globalThis.__SMUDGY_DTS_INPUT;

  // Absolute-path VFS so the default-lib `/// <reference lib>` chain resolves.
  const vfs = {};
  for (const k in input.libs) vfs["/" + k] = input.libs[k];
  for (const k in input.sources) vfs["/" + k] = input.sources[k];

  const options = {
    declaration: true,
    emitDeclarationOnly: true,
    skipLibCheck: true,
    strict: true,
    target: ts.ScriptTarget.ESNext,
    module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler,
    allowImportingTsExtensions: true,
    // JSON data modules: resolve `import x from "./data.json" with { type: "json" }` against
    // the VFS (the publish pipeline includes a package's .json modules for resolution; tsc
    // emits no .d.ts for them — only the importing module's declarations reference the shape).
    resolveJsonModule: true,
    // `.tsx` widget modules: emit via the automatic JSX runtime, resolving the JSX
    // namespace + jsx/jsxs/Fragment from the `smudgy:widgets/jsx-runtime` ambient. Only
    // affects `.tsx` sources; `.ts` emit is unchanged. A package may override per-file with
    // a `/** @jsxImportSource X */` pragma.
    jsx: ts.JsxEmit.ReactJSX,
    jsxImportSource: "smudgy:widgets",
  };

  const output = {};
  const host = {
    getSourceFile: function (fileName) {
      return vfs[fileName] !== undefined
        ? ts.createSourceFile(fileName, vfs[fileName], ts.ScriptTarget.ESNext, true)
        : undefined;
    },
    writeFile: function (fileName, data) {
      output[fileName.replace(/^\//, "")] = data;
    },
    getDefaultLibFileName: function () {
      return "/" + input.defaultLib;
    },
    getDefaultLibLocation: function () {
      return "/";
    },
    getCurrentDirectory: function () {
      return "/";
    },
    getCanonicalFileName: function (f) {
      return f;
    },
    useCaseSensitiveFileNames: function () {
      return true;
    },
    getNewLine: function () {
      return "\n";
    },
    fileExists: function (f) {
      return vfs[f] !== undefined;
    },
    readFile: function (f) {
      return vfs[f];
    },
    getDirectories: function () {
      return [];
    },
  };

  const rootNames = input.rootNames.map(function (n) {
    return "/" + n;
  });
  const program = ts.createProgram(rootNames, options, host);
  const emit = program.emit();
  const diagnostics = ts
    .getPreEmitDiagnostics(program)
    .concat(emit.diagnostics)
    .map(function (d) {
      return ts.flattenDiagnosticMessageText(d.messageText, "\n");
    });

  return JSON.stringify({ files: output, diagnostics: diagnostics });
})();
