// Loader hooks for running the package under plain Node: `smudgy:core` is a
// stub backed by globalThis.__smudgyStub, and the `smudgy://official/mudlet-db`
// dependency resolves to the sibling package folder.

const CORE = `
const S = globalThis.__smudgyStub;
export const echo = (line) => S.echoed.push(String(line));
export const getDataDir = () => S.dataDir;
export const createAlias = (pattern, script, options) => {
  const alias = { pattern, script, name: options?.name ?? String(pattern) };
  S.aliases.push(alias);
  return alias;
};
`;

const MUDLET_DB = new URL("../../mudlet-db/index.ts", import.meta.url).href;

export function resolve(specifier, context, next) {
  if (specifier === "smudgy:core") return { url: "virtual:smudgy:core", shortCircuit: true };
  if (specifier === "smudgy://official/mudlet-db") return { url: MUDLET_DB, shortCircuit: true };
  return next(specifier, context);
}

export function load(url, context, next) {
  if (url === "virtual:smudgy:core") return { format: "module", source: CORE, shortCircuit: true };
  return next(url, context);
}
