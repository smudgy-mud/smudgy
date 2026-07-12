//! Publish-time TypeScript declaration (`.d.ts`) generation.
//!
//! Runs the vendored TypeScript compiler (`vendor/typescript/`) inside a bare deno_core
//! runtime — no npm, no network, no filesystem — over a package's in-memory sources,
//! emitting `.d.ts` for its public surface. The compiler (`typescript.js`) and its
//! `lib.*.d.ts` are embedded in the binary at build time (the libs via `build.rs`), so
//! generation is fully offline and deterministic for the pinned tsc version.

use std::collections::BTreeMap;
use std::rc::Rc;

use anyhow::{Context, Result};
use deno_core::{serde_v8, FastString};
use serde::{Deserialize, Serialize};

use crate::{ModulePolicy, ScriptRuntime, ScriptRuntimeOptions};

/// The vendored TypeScript compiler, embedded at build time.
const TYPESCRIPT_JS: &str = include_str!("../vendor/typescript/lib/typescript.js");

/// The in-memory compile driver (uses the global `ts` + `__SMUDGY_DTS_INPUT`).
const DRIVER_JS: &str = include_str!("dts_driver.js");

// `build.rs` emits `pub static LIBS: &[(&str, &str)]` — every vendored `lib.*.d.ts`.
include!(concat!(env!("OUT_DIR"), "/dts_libs.rs"));

/// The default library compiled against (matches the editor tsconfig's `lib`).
const DEFAULT_LIB: &str = "lib.esnext.full.d.ts";

/// Minimal CommonJS/global shims so `typescript.js`'s UMD wrapper binds its API onto
/// `module.exports`, plus `process`/`console` fallbacks if the host doesn't provide them.
const SHIM_JS: &str = r"
globalThis.module = { exports: {} };
globalThis.exports = globalThis.module.exports;
if (typeof globalThis.process === 'undefined') {
  globalThis.process = { argv: [], env: {}, platform: 'linux', cwd: () => '/', nextTick: (f) => f() };
}
if (typeof globalThis.console === 'undefined') {
  globalThis.console = { log() {}, error() {}, warn() {}, info() {}, debug() {} };
}
";

/// Bind the loaded compiler to `globalThis.ts`.
const BIND_JS: &str = r"
globalThis.ts = (globalThis.module && globalThis.module.exports && globalThis.module.exports.createProgram)
  ? globalThis.module.exports
  : globalThis.ts;
if (!globalThis.ts || !globalThis.ts.createProgram) {
  throw new Error('failed to load the embedded TypeScript compiler');
}
";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeneratorInput<'a> {
    libs: &'a BTreeMap<String, &'static str>,
    sources: &'a BTreeMap<String, String>,
    root_names: Vec<String>,
    default_lib: &'a str,
}

/// The emitted declarations plus any compiler diagnostics.
#[derive(Debug, Deserialize)]
pub struct GeneratedDeclarations {
    /// Emitted `.d.ts`, keyed by subpath (e.g. `index.d.ts`).
    pub files: BTreeMap<String, String>,
    /// Flattened tsc diagnostic messages; empty on a clean compile.
    pub diagnostics: Vec<String>,
}

/// Generate `.d.ts` for a package's TypeScript modules.
///
/// `sources` is the package's modules keyed by subpath (e.g. `index.ts`, `util/x.ts`).
/// `ambient` is extra declaration content available during the compile but **not**
/// emitted — e.g. the `smudgy:core` ambient module so `import … from "smudgy:core"`
/// resolves. Only the `sources` produce `.d.ts` output.
///
/// # Errors
///
/// Returns an error if the embedded compiler fails to load/run or its output can't be
/// read back. tsc *type* errors are returned in [`GeneratedDeclarations::diagnostics`],
/// not as an `Err`.
pub fn generate_declarations(
    sources: &BTreeMap<String, String>,
    ambient: &BTreeMap<String, String>,
) -> Result<GeneratedDeclarations> {
    // VFS sources: the package modules (emit roots) plus ambient decls (resolve-only —
    // tsc never emits a `.d.ts` for a `.d.ts`, so ambient files don't appear in output).
    let mut all_sources: BTreeMap<String, String> = sources.clone();
    for (name, contents) in ambient {
        all_sources.insert(name.clone(), contents.clone());
    }
    let root_names: Vec<String> = all_sources.keys().cloned().collect();

    let libs: BTreeMap<String, &'static str> =
        LIBS.iter().map(|(name, body)| ((*name).to_string(), *body)).collect();

    let input = GeneratorInput {
        libs: &libs,
        sources: &all_sources,
        root_names,
        default_lib: DEFAULT_LIB,
    };
    let input_json = serde_json::to_string(&input).context("serialize dts generator input")?;

    let tokio = Rc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime for dts generation")?,
    );
    let data_dir = std::env::temp_dir().join(format!("smudgy-dts-{}", std::process::id()));
    std::fs::create_dir_all(&data_dir).ok();

    let mut rt = ScriptRuntime::new(ScriptRuntimeOptions {
        extensions: Vec::new(),
        data_dir,
        webstorage_dir: None,
        module_policy: ModulePolicy { allow_https: false, ..Default::default() },
        inspector: None,
        tokio,
        package_provider: None,
        permissions: None,
    })
    .context("construct dts generator runtime")?;

    let deno = rt.deno_runtime();
    deno.execute_script("[smudgy:dts:shim]", FastString::from(SHIM_JS.to_string()))
        .context("dts shim")?;
    deno.execute_script("[smudgy:dts:typescript]", FastString::from(TYPESCRIPT_JS.to_string()))
        .context("load typescript.js")?;
    deno.execute_script("[smudgy:dts:bind]", FastString::from(BIND_JS.to_string()))
        .context("bind ts")?;
    deno.execute_script(
        "[smudgy:dts:input]",
        FastString::from(format!("globalThis.__SMUDGY_DTS_INPUT = {input_json};")),
    )
    .context("set dts input")?;
    let value = deno
        .execute_script("[smudgy:dts:driver]", FastString::from(DRIVER_JS.to_string()))
        .context("run dts driver")?;

    let json: String = {
        deno_core::scope!(scope, rt.deno_runtime());
        let local = deno_core::v8::Local::new(scope, value);
        serde_v8::from_v8(scope, local).context("read dts driver output")?
    };
    serde_json::from_str(&json).context("parse dts driver output")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smudgy_core_ambient() -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        // Mirrors the real ambient surface enough to compile both event-typing patterns:
        // the branded `SmudgyEvent<T>` token AND the open `SmudgyEventMap` interface that
        // packages augment via declaration merging (plus the `keyof`-based `on` overload
        // that makes the merged payload type resolve). Keep this in sync with
        // `core/src/models/script_typings/smudgy-core.d.ts` so a package compiles at
        // publish-time exactly as it does in the editor.
        m.insert(
            "smudgy-core.d.ts".to_string(),
            "declare module \"smudgy:core\" {\n\
             export type SmudgyEvent<T> = string & { readonly __p?: T };\n\
             export interface SmudgyEventMap {}\n\
             export interface EventSubscription { off(): void }\n\
             export function on<K extends keyof SmudgyEventMap>(event: K, handler: (payload: SmudgyEventMap[K], name: string) => void): EventSubscription;\n\
             export function on(event: string, handler: (payload: unknown, name: string) => void): EventSubscription;\n\
             }\n"
                .to_string(),
        );
        m
    }

    #[test]
    fn generates_declarations_with_inference_and_smudgy_core() {
        let mut sources = BTreeMap::new();
        sources.insert(
            "prompt.ts".to_string(),
            "export function computeRisk(hp: number) { return hp < 100 ? \"high\" : \"low\"; }\n"
                .to_string(),
        );
        sources.insert(
            "index.ts".to_string(),
            "import type { SmudgyEvent } from \"smudgy:core\";\n\
             import { computeRisk } from \"./prompt.ts\";\n\
             export interface PromptData { hp?: number }\n\
             export const PROMPT_EVENT = \"smudgy://o/n#p\" as SmudgyEvent<PromptData>;\n\
             export function describe(hp: number) { return { level: computeRisk(hp), critical: hp < 25 }; }\n"
                .to_string(),
        );

        let out = generate_declarations(&sources, &smudgy_core_ambient()).expect("generate");
        assert!(out.diagnostics.is_empty(), "diagnostics: {:?}", out.diagnostics);

        let index = out.files.get("index.d.ts").expect("index.d.ts emitted");
        assert!(
            index.contains("PROMPT_EVENT: SmudgyEvent<PromptData>"),
            "branded token type missing:\n{index}"
        );
        assert!(index.contains("describe(hp: number)"), "describe missing:\n{index}");
        assert!(index.contains("level: string"), "inferred return missing:\n{index}");

        let prompt = out.files.get("prompt.d.ts").expect("prompt.d.ts emitted");
        assert!(prompt.contains("\"high\" | \"low\""), "inferred union missing:\n{prompt}");

        // The ambient smudgy-core.d.ts is resolve-only — not emitted as a package file.
        assert!(!out.files.contains_key("smudgy-core.d.ts"));
    }

    #[test]
    fn generates_declarations_for_json_importing_modules() {
        // JSON data modules: `import x from "./maps/x.json" with { type: "json" }` must
        // resolve against the VFS (resolveJsonModule) and type the parsed shape; the JSON
        // itself gets no `.d.ts`. This is the arctic-newbie-maps shape (per-city data files).
        let mut sources = BTreeMap::new();
        sources.insert(
            "maps/kalaman.json".to_string(),
            "{ \"name\": \"Kalaman (Newbie)\", \"rooms\": [ { \"room_number\": 1 } ] }\n"
                .to_string(),
        );
        sources.insert(
            "index.ts".to_string(),
            "import kalaman from \"./maps/kalaman.json\" with { type: \"json\" };\n\
             export const mapName: string = kalaman.name;\n\
             export const roomCount: number = kalaman.rooms.length;\n"
                .to_string(),
        );

        let out = generate_declarations(&sources, &smudgy_core_ambient()).expect("generate");
        assert!(out.diagnostics.is_empty(), "diagnostics: {:?}", out.diagnostics);

        let index = out.files.get("index.d.ts").expect("index.d.ts emitted");
        assert!(
            index.contains("mapName: string"),
            "json-typed export missing:\n{index}"
        );
        // No declaration file is emitted for the JSON data module itself.
        assert!(
            !out.files.keys().any(|k| k.contains("kalaman")),
            "unexpected declaration for a json module: {:?}",
            out.files.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn generates_declarations_with_event_map_augmentation() {
        // Option 1 typed events: a package augments the open `SmudgyEventMap` and consumes
        // its own event through the typed `on` overload. A clean compile proves the typed
        // overload resolves the payload through the merged map (under `strict`, `void p.hp`
        // on an `unknown` payload would error — so zero diagnostics means the `keyof`
        // overload won), and the assertions prove the augmentation survives into the
        // emitted `.d.ts`.
        let mut sources = BTreeMap::new();
        sources.insert(
            "index.ts".to_string(),
            "import { on } from \"smudgy:core\";\n\
             export interface PromptData { hp: number }\n\
             declare module \"smudgy:core\" {\n\
               interface SmudgyEventMap { \"smudgy://o/n#prompt\": PromptData }\n\
             }\n\
             export function wire() { on(\"smudgy://o/n#prompt\", (p) => { void p.hp; }); }\n"
                .to_string(),
        );

        let out = generate_declarations(&sources, &smudgy_core_ambient()).expect("generate");
        assert!(out.diagnostics.is_empty(), "diagnostics: {:?}", out.diagnostics);

        let index = out.files.get("index.d.ts").expect("index.d.ts emitted");
        assert!(
            index.contains("interface SmudgyEventMap"),
            "augmentation missing from emitted .d.ts:\n{index}"
        );
        assert!(
            index.contains("smudgy://o/n#prompt"),
            "augmented event key missing:\n{index}"
        );

        // The ambient smudgy-core.d.ts is resolve-only — not emitted as a package file.
        assert!(!out.files.contains_key("smudgy-core.d.ts"));
    }
}
