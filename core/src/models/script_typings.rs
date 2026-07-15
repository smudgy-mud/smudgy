//! Generates the editor project files that give script authors type-checking and
//! autocomplete for the `smudgy:core` module surface and for installed
//! `smudgy://` packages.
//!
//! smudgy scripts are authored as ES modules, and serious authors edit them in VS Code
//! — whose TypeScript language service has no idea what `smudgy:core` is. This module
//! drops a small managed TypeScript project at the **server directory**, the common
//! parent of both `modules/` (user scripts) and `packages/` (locally-authored
//! packages), so the editor types files in either subtree.
//!
//! Layout written into `<home>/<server>/`:
//!
//! ```text
//! tsconfig.json              user-facing; created only if absent, never overwritten
//! .smudgy/
//!   README.md                note that the folder is managed
//!   tsconfig.base.json       smudgy-owned compiler options; regenerated each launch
//!   types/
//!     smudgy-core.d.ts       ambient `declare module "smudgy:core"` declarations
//! modules/
//!   tsconfig.json            thin `{ "extends": "../tsconfig.json" }`; seeded only if absent
//!   …                        user scripts          (covered by the tsconfig include)
//! packages/  …               local authored packages (covered by the tsconfig include)
//! ```
//!
//! The user's `tsconfig.json` `extends` the managed base, so smudgy can refresh the
//! compiler options and type declarations on every launch without clobbering anything
//! the author added to their own config.

use crate::get_smudgy_home;
use anyhow::{Context, Result};
use include_dir::{Dir, DirEntry, include_dir};
use std::{fs, path::Path};

/// The managed ambient declarations for the `smudgy:core` module. Embedded at build
/// time, rewritten into each server's `.smudgy/types/` on session start.
const SMUDGY_CORE_DTS: &str = include_str!("script_typings/smudgy-core.d.ts");

/// The managed ambient declarations for the per-package `smudgy:params` module
/// (`get(key)` over the package's configured options).
const SMUDGY_PARAMS_DTS: &str = include_str!("script_typings/smudgy-params.d.ts");

/// The managed ambient declarations for the `mapper` API (`Mapper`/`Area`/`Room`/`Exit`/...),
/// declared as global ambient types so both bare `mapper.*` usage and `smudgy:core`'s typed
/// `mapper` member resolve them. Embedded at build time, rewritten on session start.
const SMUDGY_MAPPER_DTS: &str = include_str!("script_typings/smudgy-mapper.d.ts");

/// The managed ambient declarations for `smudgy:widgets` + `smudgy:widgets/jsx-runtime`
/// (the script-driven UI surface + the automatic-JSX runtime and its `JSX` namespace, so
/// `.tsx` authoring type-checks). Embedded at build time, rewritten on session start.
const SMUDGY_WIDGETS_DTS: &str = include_str!("script_typings/smudgy-widgets.d.ts");

/// The vendored Deno runtime lib (`Deno` namespace + web globals like `fetch`/`Response`),
/// with the `/// <reference>` directives stripped so they're plain ambient declarations.
/// Materialized to `<server>/.smudgy/types/deno/` (covered by the user tsconfig's
/// `.smudgy/types/**` include).
static DENO_LIB: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/models/script_typings/vendor/deno-lib");

/// The vendored `@types/node` tree (types `node:events`, `node:path`, … + Node globals),
/// resolved by the base tsconfig's `types`/`typeRoots`. Materialized to
/// `<server>/.smudgy/node-types/@types/node/`.
static NODE_TYPES: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/models/script_typings/vendor/node-types");

/// Bumped whenever the vendored Deno lib / `@types/node` change, so the runtime typings are
/// (re)written only on first run or after a re-vendor — not on every session start.
const RUNTIME_TYPES_VERSION: &str = "deno-v2.9.0+node-26.0.1+1";

const MANAGED_README: &str = "\
This folder is generated and managed by smudgy. It gives VS Code (and any\n\
TypeScript-aware editor) type information for the `smudgy:core` module so the\n\
scripts in `modules/` and the packages in `packages/` get autocomplete and\n\
type-checking.\n\
\n\
Everything in here is overwritten every time a smudgy session starts. Do not edit\n\
it — edit your scripts (and `../tsconfig.json`, which smudgy creates once and never\n\
overwrites) instead.\n";

/// One installed `smudgy://` package the editor project types: the tsconfig `paths` map
/// resolves `import … from "smudgy://owner/name"` to its entry module, and the
/// `smudgy:state/…` / `smudgy:events/…` consumer typings are generated from its declared
/// handles. The entry lives either in a copy materialized under
/// `<server>/.smudgy/packages/<owner>/<name>/` or — for a local dev-override — in the
/// author's live folder under `<server>/packages/<name>/`.
#[derive(Debug, Clone)]
pub struct InstalledPackageTypes {
    pub owner: String,
    pub name: String,
    /// The module the specifier resolves to, relative to the package dir: the entry *source*
    /// (e.g. `index.ts`) when available — its initializers carry the handle name literals
    /// and its `typeof` aliases carry the payload types — else the entry `.d.ts` for packages
    /// that ship only declarations.
    pub entry_module: String,
    /// The package's statically-declared interop handles (interop.md §4), extracted
    /// from the entry source; empty when only a `.d.ts` is available (no initializers).
    pub handles: Vec<smudgy_script::interop_extract::InteropHandle>,
    /// Whether this package is a local dev-override: the authored folder at
    /// `<server>/packages/<name>/` shadows the install (mirroring the resolver), so the
    /// generated paths point at that live source — payload types then track edits without
    /// a session restart — instead of a materialized copy.
    pub local: bool,
}

impl InstalledPackageTypes {
    /// The package's directory as referenced from `<server>/.smudgy/` (where the base
    /// tsconfig lives): the live authored folder for a local dev-override, else the
    /// materialized copy under `.smudgy/packages/`.
    fn dir_from_managed(&self) -> String {
        if self.local {
            format!("../packages/{}", self.name)
        } else {
            format!("./packages/{}/{}", self.owner, self.name)
        }
    }

    /// The same directory as referenced from `<server>/.smudgy/types/` (one level deeper
    /// than [`dir_from_managed`](Self::dir_from_managed)).
    fn dir_from_types(&self) -> String {
        if self.local {
            format!("../../packages/{}", self.name)
        } else {
            format!("../packages/{}/{}", self.owner, self.name)
        }
    }
}

/// Builds the managed base tsconfig. Compiler options are kept permissive so existing
/// plain-JS scripts don't light up with errors, while TS authors still get strict checks.
/// When `packages` is non-empty, a `compilerOptions.paths` block maps each
/// `smudgy://owner/name` (and `…/*` subpaths) to its materialized `.d.ts` — resolved
/// relative to this file's `.smudgy/` directory.
fn tsconfig_base(packages: &[InstalledPackageTypes]) -> Result<String> {
    let mut compiler_options = serde_json::json!({
        "target": "ESNext",
        "module": "ESNext",
        "moduleResolution": "Bundler",
        "lib": ["ESNext"],
        "types": ["node"],
        "typeRoots": ["./node-types/@types"],
        // JSON data modules (`import maps from "./maps/x.json" with { type: "json" }`) are
        // first-class at runtime (the loader serves ModuleType::Json); this types them in the
        // editor, inferring the literal shape of the file.
        "resolveJsonModule": true,
        // The runtime transpiles file-at-a-time (swc type-stripping, no cross-file type info),
        // so patterns that need whole-program knowledge — re-exporting a type without
        // `export type`, const enums — break at runtime while a non-isolated check stays
        // green. This makes editor diagnostics match what the runtime can execute.
        "isolatedModules": true,
        "allowJs": true,
        "checkJs": false,
        "noEmit": true,
        "strict": true,
        "skipLibCheck": true,
        "esModuleInterop": true,
        "allowImportingTsExtensions": true,
        "forceConsistentCasingInFileNames": true,
        // `.tsx` widget authoring: the automatic JSX runtime resolves to the
        // `smudgy:widgets/jsx-runtime` ambient module (jsx/jsxs/Fragment + the `JSX` namespace).
        "jsx": "react-jsx",
        "jsxImportSource": "smudgy:widgets"
    });
    if !packages.is_empty() {
        let mut paths = serde_json::Map::new();
        for pkg in packages {
            paths.insert(
                format!("smudgy://{}/{}", pkg.owner, pkg.name),
                serde_json::json!([format!("{}/{}", pkg.dir_from_managed(), pkg.entry_module)]),
            );
            paths.insert(
                format!("smudgy://{}/{}/*", pkg.owner, pkg.name),
                serde_json::json!([format!("{}/*", pkg.dir_from_managed())]),
            );
        }
        compiler_options["paths"] = serde_json::Value::Object(paths);
    }
    let base = serde_json::json!({ "compilerOptions": compiler_options });
    let body = serde_json::to_string_pretty(&base).context("serialize tsconfig.base.json")?;
    Ok(format!(
        "// GENERATED BY SMUDGY — regenerated on every session start; do not edit.\n\
         // Shared compiler options for smudgy scripts; your ../tsconfig.json extends this.\n\
         {body}\n"
    ))
}

/// Header for the generated `installed-events.d.ts` barrel (below). The `paths` map
/// in the base tsconfig only loads a package's entry module when its specifier is
/// *imported*; a `/// <reference path>` pulls it into the program unconditionally, so
/// the package's exported types are discoverable (and any legacy `declare module`
/// augmentation still applies) before any consumer imports it. The filename predates
/// the handle-based interop surface and is kept so older on-disk copies are
/// overwritten rather than left dangling beside a renamed twin.
const INSTALLED_EVENTS_HEADER: &str = "\
// =============================================================================\n\
//  smudgy installed-package typings barrel  (GENERATED — DO NOT EDIT)\n\
// =============================================================================\n\
//  smudgy writes and overwrites this file every time a session starts. Each\n\
//  reference below pulls an installed `smudgy://` package's entry module into\n\
//  the TypeScript program so its exported types are discoverable before you\n\
//  import it anywhere.\n\
//\n\
//  Edits here are lost on the next launch.\n\
// =============================================================================\n\
";

/// Builds the `installed-events.d.ts` barrel: one `/// <reference path>` per installed
/// package entry module, relative to `.smudgy/types/`. References are emitted in a
/// stable `(owner, name)` order so the file is byte-stable across runs (so
/// [`write_if_changed`] no-ops and the editor doesn't reload the project when nothing
/// changed). The barrel is fully regenerated each session, so a shrinking install set
/// self-prunes — removed packages' references simply vanish, and an empty set yields a
/// header-only file.
fn installed_events_barrel(packages: &[InstalledPackageTypes]) -> String {
    use std::fmt::Write as _;
    let mut pkgs: Vec<&InstalledPackageTypes> = packages.iter().collect();
    pkgs.sort_by(|a, b| a.owner.cmp(&b.owner).then_with(|| a.name.cmp(&b.name)));
    let mut out = String::from(INSTALLED_EVENTS_HEADER);
    for pkg in pkgs {
        // `entry_module` is already forward-slashed; TS reference paths use `/` on every
        // platform. Writing to a `String` is infallible, so the `Result` is discarded.
        let _ = writeln!(
            out,
            "/// <reference path=\"{}/{}\" />",
            pkg.dir_from_types(),
            pkg.entry_module
        );
    }
    out
}

/// Header for the generated `interop-handles.d.ts` (below).
const INTEROP_HANDLES_HEADER: &str = "\
// =============================================================================\n\
//  smudgy interop consumer typings  (GENERATED — DO NOT EDIT)\n\
// =============================================================================\n\
//  smudgy writes and overwrites this file every time a session starts. It types\n\
//  the `smudgy:state/...` / `smudgy:events/...` / `smudgy:procedures/...` modules\n\
//  for each installed or locally-authored package's declared handles: payload\n\
//  types flow from `typeof` the package's exported handle declarations, so\n\
//  renaming a field in the producer's source re-types every consumer\n\
//  immediately. Each handle is exported as a value AND a same-named payload\n\
//  type, so `function f(a: evt)` works without naming anything else.\n\
//\n\
//  Edits here are lost on the next launch.\n\
// =============================================================================\n\
";

/// Builds `interop-handles.d.ts`: per installed package with declared handles, a
/// `declare module` block per kind scheme (`smudgy:state/<owner>/<name>`,
/// `smudgy:events/<owner>/<name>`, `smudgy:procedures/<owner>/<name>`) whose exports are the
/// handle *name strings* (interop.md §4 naming rules), typed through
/// `ConsumerOf<typeof import(entry).const>` for exported handles (the declaration IS the
/// type source), through the erased `typeof` alias for module-local handles that export
/// one, and as `unknown`-payload consumers otherwise (JS packages are first-class). Each
/// handle also gets (interop.md §5):
/// - a **twin type export** under the handle's own name (`export type vitals = Payload<…>`),
///   so a named handler writes `function f(v: vitals)`;
/// - its producer doc comment, so consumer-side hover shows the producer's documentation;
/// - a re-export of the author's payload type when the entry exports one;
/// - a single-handle subpath module mirroring the runtime's subpath form, with a fixed
///   `Payload` type export.
///
/// Emitted in stable `(owner, name)` order, like [`installed_events_barrel`].
fn interop_handles_dts(packages: &[InstalledPackageTypes]) -> String {
    use smudgy_script::interop_extract::InteropKind;
    use std::fmt::Write as _;

    let mut pkgs: Vec<&InstalledPackageTypes> = packages.iter().collect();
    pkgs.sort_by(|a, b| a.owner.cmp(&b.owner).then_with(|| a.name.cmp(&b.name)));
    let mut out = String::from(INTEROP_HANDLES_HEADER);
    for pkg in pkgs {
        for (kind, scheme) in [
            (InteropKind::State, "state"),
            (InteropKind::Event, "events"),
            (InteropKind::Procedure, "procedures"),
        ] {
            let handles: Vec<_> = pkg.handles.iter().filter(|h| h.kind == kind).collect();
            if handles.is_empty() {
                continue;
            }
            let module = format!("smudgy:{scheme}/{}/{}", pkg.owner, pkg.name);
            let twin_names: std::collections::HashSet<&str> = handles
                .iter()
                .filter(|h| is_type_alias_name(&h.name))
                .map(|h| h.name.as_str())
                .collect();
            let _ = writeln!(out, "\ndeclare module {} {{", quote_ts(&module));
            for (index, handle) in handles.iter().enumerate() {
                if let Some(doc) = &handle.doc {
                    let _ = writeln!(out, "  {doc}");
                }
                let consumer_type = consumer_type_for(pkg, handle);
                let _ = writeln!(out, "  const __h{index}: {consumer_type};");
                let _ = writeln!(
                    out,
                    "  export {{ __h{index} as {} }};",
                    export_name(&handle.name)
                );
                // The twin: the same identifier, in type position, is the handle's payload —
                // `import { evt } …; function f(a: evt)` (interop.md §5). Only spellable for
                // identifier names that are legal type-alias names (a type alias can carry
                // neither a string-literal name nor a reserved word).
                if is_type_alias_name(&handle.name) {
                    let _ = writeln!(
                        out,
                        "  export type {} = import(\"smudgy:core\").Payload<typeof __h{index}>;",
                        handle.name
                    );
                }
            }
            // The author's exported payload types, importable from the module consumers
            // already import from. A name collision with a handle's twin skips the
            // re-export — the twin is the primary spelling.
            let mut re_exports: Vec<&str> = handles
                .iter()
                .filter_map(|h| h.payload_type_export.as_deref())
                .filter(|name| !twin_names.contains(name))
                .collect();
            re_exports.sort_unstable();
            re_exports.dedup();
            for name in re_exports {
                let _ = writeln!(
                    out,
                    "  export type {{ {name} }} from {};",
                    quote_ts(&format!("smudgy://{}/{}", pkg.owner, pkg.name))
                );
            }
            let _ = writeln!(out, "}}");
            // The single-handle subpath form, typed off the whole-module export above so the
            // two spellings can never drift; `Payload` is its fixed-name payload export.
            for handle in &handles {
                let submodule = format!("{module}/{}", handle.name);
                let _ = writeln!(out, "\ndeclare module {} {{", quote_ts(&submodule));
                if let Some(doc) = &handle.doc {
                    let _ = writeln!(out, "  {doc}");
                }
                let _ = writeln!(
                    out,
                    "  const __handle: (typeof import({}))[{}];",
                    quote_ts(&module),
                    quote_ts(&handle.name)
                );
                let _ = writeln!(out, "  export {{ __handle as {} }};", export_name(&handle.name));
                let _ = writeln!(out, "  export default __handle;");
                let _ = writeln!(
                    out,
                    "  export type Payload = import(\"smudgy:core\").Payload<typeof __handle>;"
                );
                let _ = writeln!(out, "}}");
            }
        }
    }
    out
}

/// The consumer-side type of one handle. Exported handles derive from `typeof` the
/// declaration itself — the strongest link to the author's source; module-local handles
/// with an erased `typeof` alias derive from the alias; anything else (a JS package with no
/// type information at all still gets working handles) falls back to an `unknown`-payload
/// consumer of the right kind.
fn consumer_type_for(
    pkg: &InstalledPackageTypes,
    handle: &smudgy_script::interop_extract::InteropHandle,
) -> String {
    use smudgy_script::interop_extract::InteropKind;
    let entry = quote_ts(&format!("smudgy://{}/{}", pkg.owner, pkg.name));
    if handle.exported && is_ts_ident(&handle.const_name) {
        return format!(
            "import(\"smudgy:core\").ConsumerOf<typeof import({entry}).{}>",
            handle.const_name
        );
    }
    match &handle.type_alias {
        Some(alias) => format!("import(\"smudgy:core\").ConsumerOf<import({entry}).{alias}>"),
        None => match handle.kind {
            InteropKind::State => "import(\"smudgy:core\").StateConsumer<unknown>".to_string(),
            InteropKind::Event => "import(\"smudgy:core\").EventConsumer<unknown>".to_string(),
            InteropKind::Procedure => "import(\"smudgy:core\").ProcedureConsumer<unknown>".to_string(),
        },
    }
}

/// Whether `name` can appear as a bare TS identifier (property access, export clauses) —
/// the extractor's shared spellability test.
fn is_ts_ident(name: &str) -> bool {
    smudgy_script::interop_extract::is_ident_name(name)
}

/// Whether `name` can be a TS *type-alias* name: a bare identifier that is not a reserved
/// word or built-in type name. A handle explicitly named `class` or `string` is legal at
/// runtime (the value export rides a string-literal export clause), but emitting
/// `export type class = …` would be a parse error poisoning the whole generated file — such
/// a handle simply gets no twin.
fn is_type_alias_name(name: &str) -> bool {
    const UNSPELLABLE: &[&str] = &[
        // Reserved words (parse errors as type-alias names).
        "break", "case", "catch", "class", "const", "continue", "debugger", "default",
        "delete", "do", "else", "enum", "export", "extends", "false", "finally", "for",
        "function", "if", "import", "in", "instanceof", "new", "null", "return", "super",
        "switch", "this", "throw", "true", "try", "typeof", "var", "void", "while", "with",
        "implements", "interface", "let", "package", "private", "protected", "public",
        "static", "yield", "await",
        // Built-in type names tsc refuses as alias names.
        "any", "unknown", "never", "object", "string", "number", "boolean", "symbol",
        "bigint", "undefined", "intrinsic",
    ];
    is_ts_ident(name) && !UNSPELLABLE.contains(&name)
}

/// Spell a handle name as an export-clause name: bare identifiers stay bare; anything
/// else uses the string-literal module-export-name form (`export { x as "…" }`).
fn export_name(name: &str) -> String {
    if is_ts_ident(name) {
        name.to_string()
    } else {
        quote_ts(name)
    }
}

/// JSON-escape a string into a double-quoted TS string literal.
fn quote_ts(value: &str) -> String {
    serde_json::to_string(value).expect("a string always serializes")
}

/// The user-facing tsconfig. Written only when absent; the author owns it thereafter.
/// It covers both `modules/` and `packages/`; the explicit `.smudgy/types` entry is
/// required because TypeScript's `**` globs skip dot-directories.
const TSCONFIG_USER: &str = r#"{
  // Created by smudgy (only when absent — your edits here are preserved).
  // Shared settings live in ./.smudgy/tsconfig.base.json, which smudgy regenerates
  // on each launch. Add your own compilerOptions / include here as you like.
  "extends": "./.smudgy/tsconfig.base.json",
  "include": [
    "modules/**/*",
    "packages/**/*",
    ".smudgy/types/**/*.d.ts"
  ]
}
"#;

/// Substring that marks a `tsconfig.json` as smudgy-generated (vs. author-written), used
/// when removing the stale `modules/`-level project.
const USER_TSCONFIG_MARKER: &str = "./.smudgy/tsconfig.base.json";

/// The seeded `.vscode/settings.json`. Written only when absent; the author owns it
/// thereafter. Excluding installed package sources from auto-import steers the editor
/// toward the `smudgy:state/…` / `smudgy:events/…` consumer modules for handle symbols.
const VSCODE_SETTINGS: &str = r#"{
  // Created by smudgy (only when absent — your edits here are preserved).
  // Auto-import suggestions skip installed package sources: consume a package's
  // state/events via the smudgy:state/... and smudgy:events/... modules instead.
  "typescript.preferences.autoImportFileExcludePatterns": ["packages/**"]
}
"#;

/// The thin `modules/tsconfig.json`: it merely `extends` the server-level project one directory
/// up, so the `modules/` subtree resolves smudgy types even when opened on its own in an editor.
/// Seeded only when absent (the author owns it thereafter); a heavier author-written or stale
/// generated config is left to [`migrate_modules_level_project`].
const TSCONFIG_MODULES: &str = "{ \"extends\": \"../tsconfig.json\" }\n";

/// Ensures the managed TypeScript project files exist at `<server>`'s directory so
/// editors can type smudgy scripts in both `modules/` and `packages/`. Best-effort and
/// idempotent.
///
/// The managed files (`.smudgy/tsconfig.base.json`, `.smudgy/types/*.d.ts`,
/// `.smudgy/README.md`) are (re)written to match this build. The user-facing
/// `tsconfig.json` is created only when missing and never overwritten.
///
/// # Errors
///
/// Returns an error if the smudgy home directory can't be resolved or the managed files
/// can't be written.
pub fn ensure_script_tsconfig(server_name: &str) -> Result<()> {
    ensure_script_tsconfig_with_packages(server_name, &[])
}

/// Like [`ensure_script_tsconfig`], but also wires `compilerOptions.paths` for the given
/// packages — a materialized copy under `<server>/.smudgy/packages/<owner>/<name>/`, or
/// the live authored folder for a local dev-override — so the editor types
/// `import … from "smudgy://owner/name"`.
///
/// # Errors
///
/// Returns an error if the smudgy home directory can't be resolved or the managed files
/// can't be written.
pub fn ensure_script_tsconfig_with_packages(
    server_name: &str,
    packages: &[InstalledPackageTypes],
) -> Result<()> {
    let server_dir = get_smudgy_home()?.join(server_name);
    ensure_script_tsconfig_in(&server_dir, packages)
}

/// [`ensure_script_tsconfig_with_packages`] against an explicit server directory (test seam).
fn ensure_script_tsconfig_in(server_dir: &Path, packages: &[InstalledPackageTypes]) -> Result<()> {
    let managed_dir = server_dir.join(".smudgy");
    let types_dir = managed_dir.join("types");
    fs::create_dir_all(&types_dir).with_context(|| format!("create {}", types_dir.display()))?;

    write_if_changed(&managed_dir.join("README.md"), MANAGED_README)?;
    write_if_changed(&managed_dir.join("tsconfig.base.json"), &tsconfig_base(packages)?)?;
    write_if_changed(&types_dir.join("smudgy-core.d.ts"), SMUDGY_CORE_DTS)?;
    write_if_changed(&types_dir.join("smudgy-params.d.ts"), SMUDGY_PARAMS_DTS)?;
    write_if_changed(&types_dir.join("smudgy-mapper.d.ts"), SMUDGY_MAPPER_DTS)?;
    write_if_changed(&types_dir.join("smudgy-widgets.d.ts"), SMUDGY_WIDGETS_DTS)?;
    write_if_changed(
        &types_dir.join("installed-events.d.ts"),
        &installed_events_barrel(packages),
    )?;
    write_if_changed(
        &types_dir.join("interop-handles.d.ts"),
        &interop_handles_dts(packages),
    )?;
    ensure_runtime_types(&managed_dir)?;

    // The user's tsconfig is theirs once created — only seed it when absent.
    let user_tsconfig = server_dir.join("tsconfig.json");
    if !user_tsconfig.exists() {
        fs::write(&user_tsconfig, TSCONFIG_USER)
            .with_context(|| format!("write {}", user_tsconfig.display()))?;
    }

    // Auto-import steering (interop.md §5): suggest handle symbols from the smudgy:state/… /
    // smudgy:events/… modules, not the installed package sources. Seeded only when absent —
    // the settings file is the author's once it exists. Purely steering: a direct code
    // import is not dangerous (writes are home-gated regardless).
    let vscode_dir = server_dir.join(".vscode");
    let vscode_settings = vscode_dir.join("settings.json");
    if !vscode_settings.exists() {
        fs::create_dir_all(&vscode_dir)
            .with_context(|| format!("create {}", vscode_dir.display()))?;
        fs::write(&vscode_settings, VSCODE_SETTINGS)
            .with_context(|| format!("write {}", vscode_settings.display()))?;
    }

    migrate_modules_level_project(server_dir);
    ensure_modules_tsconfig(server_dir)?;

    Ok(())
}

/// Seeds the thin `modules/tsconfig.json` (and the `modules/` directory itself) so the subtree is
/// a self-contained TypeScript project pointing at the server-level config. Written only when
/// absent — and *after* [`migrate_modules_level_project`], so a freshly removed stale generated
/// stub is replaced by this pointer rather than the pointer being mistaken for the stale stub.
fn ensure_modules_tsconfig(server_dir: &Path) -> Result<()> {
    let modules_dir = server_dir.join("modules");
    fs::create_dir_all(&modules_dir).with_context(|| format!("create {}", modules_dir.display()))?;
    let tsconfig = modules_dir.join("tsconfig.json");
    if !tsconfig.exists() {
        fs::write(&tsconfig, TSCONFIG_MODULES)
            .with_context(|| format!("write {}", tsconfig.display()))?;
    }
    Ok(())
}

/// Removes a stale `modules/`-level project. The managed project lives at the server
/// directory (so it also covers `packages/`), so any `modules/.smudgy` and any
/// smudgy-generated `modules/tsconfig.json` stub are deleted. Best-effort: a
/// `modules/tsconfig.json` the author wrote themselves (no smudgy marker) is left alone.
fn migrate_modules_level_project(server_dir: &Path) {
    let old_managed = server_dir.join("modules").join(".smudgy");
    if old_managed.is_dir() {
        let _ = fs::remove_dir_all(&old_managed);
    }
    let old_user = server_dir.join("modules").join("tsconfig.json");
    if fs::read_to_string(&old_user).is_ok_and(|c| c.contains(USER_TSCONFIG_MARKER)) {
        let _ = fs::remove_file(&old_user);
    }
}

/// Writes `contents` to `path` only when the file is absent or differs, so a session
/// start doesn't needlessly bump file mtimes (which would make editors re-load the type
/// project) when nothing changed.
fn write_if_changed(path: &Path, contents: &str) -> Result<()> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    fs::write(path, contents).with_context(|| format!("write {}", path.display()))
}

/// Writes the vendored editor runtime typings (Deno lib + `@types/node`) into the managed
/// `.smudgy/` project. Gated by [`RUNTIME_TYPES_VERSION`]: the ~100 vendored files are
/// (re)written only on first run or after a re-vendor, not on every session start.
fn ensure_runtime_types(managed_dir: &Path) -> Result<()> {
    let marker = managed_dir.join(".runtime-types-version");
    if fs::read_to_string(&marker).is_ok_and(|v| v.trim() == RUNTIME_TYPES_VERSION) {
        return Ok(());
    }
    write_embedded_dir(&managed_dir.join("types").join("deno"), &DENO_LIB)?;
    write_embedded_dir(&managed_dir.join("node-types"), &NODE_TYPES)?;
    fs::write(&marker, RUNTIME_TYPES_VERSION).with_context(|| format!("write {}", marker.display()))
}

/// Recursively writes every file in an embedded [`Dir`] under `target`, preserving the
/// embedded directory structure (each file's path is relative to the embedded root).
fn write_embedded_dir(target: &Path, dir: &Dir<'_>) -> Result<()> {
    for entry in dir.entries() {
        match entry {
            DirEntry::Dir(sub) => write_embedded_dir(target, sub)?,
            DirEntry::File(file) => {
                let path = target.join(file.path());
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("create {}", parent.display()))?;
                }
                if fs::read(&path).is_ok_and(|existing| existing == file.contents()) {
                    continue;
                }
                fs::write(&path, file.contents())
                    .with_context(|| format!("write {}", path.display()))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_server_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smudgy-tsconfig-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn writes_managed_files_and_seeds_user_tsconfig() {
        let dir = temp_server_dir("writes");
        ensure_script_tsconfig_in(&dir, &[]).expect("ensure");

        assert!(dir.join("tsconfig.json").is_file());
        assert!(dir.join(".smudgy/tsconfig.base.json").is_file());
        assert!(dir.join(".smudgy/types/smudgy-core.d.ts").is_file());
        assert!(dir.join(".smudgy/types/smudgy-params.d.ts").is_file());
        assert!(dir.join(".smudgy/types/smudgy-mapper.d.ts").is_file());
        assert!(dir.join(".smudgy/types/smudgy-widgets.d.ts").is_file());
        assert!(dir.join(".smudgy/README.md").is_file());

        let core = fs::read_to_string(dir.join(".smudgy/types/smudgy-core.d.ts")).unwrap();
        assert!(core.contains("declare module \"smudgy:core\""));
        // The interop handle surface + the platform event catalog modules.
        assert!(core.contains("export function createState<"));
        assert!(core.contains("export function createEvent<"));
        assert!(core.contains("declare module \"smudgy:events/sys\""));
        assert!(core.contains("declare module \"smudgy:events/map\""));
        // The session/mapper named exports added in the typings refresh.
        assert!(core.contains("export const session: Session"));
        assert!(core.contains("export function getSessions()"));
        assert!(core.contains("export const mapper: Mapper"));

        // The mapper + widgets ambient declarations ship alongside.
        let mapper = fs::read_to_string(dir.join(".smudgy/types/smudgy-mapper.d.ts")).unwrap();
        assert!(mapper.contains("interface Mapper"));
        assert!(mapper.contains("declare const mapper: Mapper"));
        let widgets = fs::read_to_string(dir.join(".smudgy/types/smudgy-widgets.d.ts")).unwrap();
        assert!(widgets.contains("declare module \"smudgy:widgets\""));
        assert!(widgets.contains("declare module \"smudgy:widgets/jsx-runtime\""));
        assert!(widgets.contains("namespace JSX"));

        // The base tsconfig wires the automatic JSX runtime for `.tsx` widget authoring.
        let base = fs::read_to_string(dir.join(".smudgy/tsconfig.base.json")).unwrap();
        assert!(base.contains("react-jsx"), "jsx setting missing:\n{base}");
        assert!(base.contains("smudgy:widgets"), "jsxImportSource missing:\n{base}");

        // The installed-events barrel is always written (header-only when no packages).
        assert!(dir.join(".smudgy/types/installed-events.d.ts").is_file());

        // The seeded tsconfig covers both subtrees.
        let user = fs::read_to_string(dir.join("tsconfig.json")).unwrap();
        assert!(user.contains("./.smudgy/tsconfig.base.json"));
        assert!(user.contains("modules/**/*"));
        assert!(user.contains("packages/**/*"));

        // The `modules/` subtree is seeded as its own thin project pointing one level up.
        let modules_ts = fs::read_to_string(dir.join("modules/tsconfig.json")).unwrap();
        assert!(modules_ts.contains("../tsconfig.json"), "modules tsconfig extends the server project:\n{modules_ts}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preserves_existing_user_tsconfig() {
        let dir = temp_server_dir("preserve");
        let user = dir.join("tsconfig.json");
        let original = "{ \"my\": \"custom config\" }";
        fs::write(&user, original).unwrap();

        ensure_script_tsconfig_in(&dir, &[]).expect("ensure");

        // The author's tsconfig is untouched, but the managed base is still written.
        assert_eq!(fs::read_to_string(&user).unwrap(), original);
        assert!(dir.join(".smudgy/tsconfig.base.json").is_file());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn base_tsconfig_wires_paths_for_installed_packages() {
        let dir = temp_server_dir("paths");
        let packages = [InstalledPackageTypes {
            owner: "kapusniak".to_string(),
            name: "arctic-prompt".to_string(),
            entry_module: "index.ts".to_string(),
            handles: Vec::new(),
            local: false,
        }];
        ensure_script_tsconfig_in(&dir, &packages).expect("ensure");

        let base = fs::read_to_string(dir.join(".smudgy/tsconfig.base.json")).unwrap();
        assert!(base.contains("\"paths\""), "paths block missing:\n{base}");
        assert!(
            base.contains("\"smudgy://kapusniak/arctic-prompt\""),
            "specifier path missing:\n{base}"
        );
        assert!(
            base.contains("./packages/kapusniak/arctic-prompt/index.ts"),
            "entry module target missing:\n{base}"
        );
        assert!(
            base.contains("\"smudgy://kapusniak/arctic-prompt/*\""),
            "subpath wildcard missing:\n{base}"
        );

        // With no packages, the base carries no `paths`.
        ensure_script_tsconfig_in(&dir, &[]).expect("ensure empty");
        let bare = fs::read_to_string(dir.join(".smudgy/tsconfig.base.json")).unwrap();
        assert!(!bare.contains("\"paths\""), "unexpected paths block:\n{bare}");

        fs::remove_dir_all(&dir).ok();
    }

    /// A local dev-override package's typings point at the live authored folder under
    /// `<server>/packages/<name>/` (no owner segment on disk), not at a materialized copy
    /// under `.smudgy/packages/` — both in the tsconfig `paths` map and the reference
    /// barrel — so payload types track the author's edits without a session restart.
    #[test]
    fn local_packages_resolve_to_the_live_authored_folder() {
        let dir = temp_server_dir("local-paths");
        let packages = [InstalledPackageTypes {
            owner: "kapusniak".to_string(),
            name: "arctic-prompt".to_string(),
            entry_module: "index.ts".to_string(),
            handles: Vec::new(),
            local: true,
        }];
        ensure_script_tsconfig_in(&dir, &packages).expect("ensure");

        let base = fs::read_to_string(dir.join(".smudgy/tsconfig.base.json")).unwrap();
        assert!(
            base.contains("../packages/arctic-prompt/index.ts"),
            "entry must resolve to the live folder:\n{base}"
        );
        assert!(
            base.contains("../packages/arctic-prompt/*"),
            "subpaths must resolve to the live folder:\n{base}"
        );
        assert!(
            !base.contains("packages/kapusniak"),
            "a local package must not point into .smudgy/packages/:\n{base}"
        );

        let barrel = fs::read_to_string(dir.join(".smudgy/types/installed-events.d.ts")).unwrap();
        assert!(
            barrel.contains("/// <reference path=\"../../packages/arctic-prompt/index.ts\" />"),
            "barrel must reference the live folder:\n{barrel}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writes_installed_events_barrel() {
        let dir = temp_server_dir("events-barrel");
        let packages = [InstalledPackageTypes {
            owner: "kapusniak".to_string(),
            name: "arctic-prompt".to_string(),
            entry_module: "index.ts".to_string(),
            handles: Vec::new(),
            local: false,
        }];
        ensure_script_tsconfig_in(&dir, &packages).expect("ensure");

        let barrel = fs::read_to_string(dir.join(".smudgy/types/installed-events.d.ts")).unwrap();
        assert!(
            barrel
                .contains("/// <reference path=\"../packages/kapusniak/arctic-prompt/index.ts\" />"),
            "package reference missing:\n{barrel}"
        );

        // Re-running with an empty set fully regenerates the barrel — the stale reference
        // self-prunes, leaving a header-only file.
        ensure_script_tsconfig_in(&dir, &[]).expect("ensure empty");
        let bare = fs::read_to_string(dir.join(".smudgy/types/installed-events.d.ts")).unwrap();
        assert!(!bare.contains("arctic-prompt"), "stale reference lingered:\n{bare}");

        fs::remove_dir_all(&dir).ok();
    }

    /// The generated `interop-handles.d.ts`: whole-module + subpath `declare module` blocks
    /// per kind, exports under the handle *name strings*, `ConsumerOf<alias>` typing when the
    /// producer exports an erased alias and `unknown`-payload consumers when it doesn't (JS
    /// packages) — and the generated shim must itself compile against the real contract with
    /// a typed consumer using it.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn generates_interop_handle_shims_that_compile() {
        use smudgy_script::interop_extract::{InteropHandle, InteropKind};
        use std::collections::BTreeMap;

        let packages = [InstalledPackageTypes {
            owner: "kapusniak".to_string(),
            name: "arctic-prompt".to_string(),
            entry_module: "index.ts".to_string(),
            handles: vec![
                // Exported handle: the primary path — payload from `typeof` the declaration.
                InteropHandle {
                    kind: InteropKind::State,
                    name: "promptState".to_string(),
                    const_name: "promptState".to_string(),
                    exported: true,
                    type_alias: None,
                    declared_shape: Some("PromptData".to_string()),
                    payload_type_export: Some("PromptData".to_string()),
                    doc: Some("/** The current prompt reading. */".to_string()),
                },
                // Alias-less, unexported (a JS package): unknown payload, still typed names.
                InteropHandle {
                    kind: InteropKind::Event,
                    name: "prompt".to_string(),
                    const_name: "prompt".to_string(),
                    exported: false,
                    type_alias: None,
                    declared_shape: None,
                    payload_type_export: None,
                    doc: None,
                },
                // Module-local with an erased alias: the pre-export-handles pattern.
                InteropHandle {
                    kind: InteropKind::Procedure,
                    name: "refreshRequest".to_string(),
                    const_name: "refreshRequest".to_string(),
                    exported: false,
                    type_alias: Some("RefreshRequest".to_string()),
                    declared_shape: None,
                    payload_type_export: None,
                    doc: None,
                },
            ],
            local: false,
        }];
        let shims = interop_handles_dts(&packages);
        assert!(shims.contains("declare module \"smudgy:state/kapusniak/arctic-prompt\""));
        assert!(shims.contains("declare module \"smudgy:state/kapusniak/arctic-prompt/promptState\""));
        assert!(shims.contains("declare module \"smudgy:events/kapusniak/arctic-prompt\""));
        assert!(shims.contains("declare module \"smudgy:procedures/kapusniak/arctic-prompt\""));
        assert!(
            shims.contains("ConsumerOf<typeof import(\"smudgy://kapusniak/arctic-prompt\").promptState>"),
            "an exported handle derives from typeof its declaration:\n{shims}"
        );
        assert!(
            shims.contains("ConsumerOf<import(\"smudgy://kapusniak/arctic-prompt\").RefreshRequest>"),
            "a module-local handle still derives via its erased alias:\n{shims}"
        );
        assert!(
            shims.contains("EventConsumer<unknown>"),
            "alias-less handle must fall back to an unknown payload:\n{shims}"
        );
        assert!(
            shims.contains("export type promptState = import(\"smudgy:core\").Payload<typeof __h0>;"),
            "the twin type export rides the handle's own name:\n{shims}"
        );
        assert!(
            shims.contains("export type { PromptData } from \"smudgy://kapusniak/arctic-prompt\";"),
            "the author's exported payload type is re-exported:\n{shims}"
        );
        assert!(
            shims.contains("/** The current prompt reading. */"),
            "producer doc comments propagate:\n{shims}"
        );
        assert!(
            shims.contains("export type Payload = import(\"smudgy:core\").Payload<typeof __handle>;"),
            "subpath modules export the fixed-name Payload:\n{shims}"
        );

        // The shim + a consumer must compile against the real contract. The producer module
        // stands in for the materialized entry source `smudgy://…` resolves to.
        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert("smudgy-mapper.d.ts".to_string(), SMUDGY_MAPPER_DTS.to_string());
        ambient.insert("interop-handles.d.ts".to_string(), shims);
        ambient.insert(
            "producer-stub.d.ts".to_string(),
            "declare module \"smudgy://kapusniak/arctic-prompt\" {\n\
               import type { StateHandle, ProcedureHandle } from \"smudgy:core\";\n\
               export interface PromptData { hp: number }\n\
               export const promptState: StateHandle<PromptData>;\n\
               export type RefreshRequest = ProcedureHandle<{ full: boolean }>;\n\
             }\n"
                .to_string(),
        );
        let mut sources = BTreeMap::new();
        sources.insert(
            "consumer.ts".to_string(),
            "import { promptState } from \"smudgy:state/kapusniak/arctic-prompt\";\n\
             import type { promptState as promptStateT, PromptData } from \"smudgy:state/kapusniak/arctic-prompt\";\n\
             import promptStateDefault, { type Payload as PromptPayload } from \"smudgy:state/kapusniak/arctic-prompt/promptState\";\n\
             import { prompt } from \"smudgy:events/kapusniak/arctic-prompt\";\n\
             import { refreshRequest } from \"smudgy:procedures/kapusniak/arctic-prompt\";\n\
             // The twin: the handle's own name IS its payload type for named handlers.\n\
             function onPrompt(v: promptStateT | undefined) { void v?.hp; }\n\
             function viaSubpath(v: PromptPayload | undefined) { void v?.hp; }\n\
             function viaReExport(v: PromptData) { void v.hp; }\n\
             export function wire() {\n\
               const hp: number | undefined = promptState.value?.hp; void hp;\n\
               const prev: number | undefined = promptState.previousValue?.hp; void prev;\n\
               const same: number | undefined = promptStateDefault.value?.hp; void same;\n\
               promptState.watch(onPrompt).off();\n\
               promptState.watch((next) => { viaSubpath(next); if (next) viaReExport(next); }).off();\n\
               promptState.onWrite((path, snapshot) => { const p: string = path; void p; void snapshot; }).off();\n\
               prompt.on((p) => { void p; }).off();\n\
               refreshRequest.post({ full: true });\n\
             }\n"
                .to_string(),
        );
        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("compile a consumer against the generated shims");
        assert!(
            out.diagnostics.is_empty(),
            "generated interop shims produced diagnostics: {:?}",
            out.diagnostics
        );
    }

    /// The declaration-grammar conformance fixture (interop.md §4/§5, interop-refinement-plan
    /// task 9): FOUR independent mechanisms parse producer declarations — transpile-time name
    /// injection, static extraction, the typings generator, and the non-home scrub. One
    /// golden source, asserted against all four, so they can never drift on what a
    /// declaration means.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn declaration_grammar_conformance_across_all_consumers() {
        use smudgy_script::interop_extract::{
            extract_interop_handles, inject_inferred_handle_names, scrub_handle_exports,
            InteropKind,
        };

        const FIXTURE: &str = r#"
import { createState, createEvent, createProcedure, createDerived } from "smudgy:core";
export interface VitalData { hp: number }

/** The current vitals reading. */
export const vitals = createState<VitalData>();
export const prompt = createEvent<{ raw: string }>();
export const refresh = createProcedure((args: { full: boolean }, sender: string) => { void args; void sender; });
export const hpPct = createDerived(vitals as any, (v: any) => v.hp);
const pinned = createState<VitalData>('Pinned');
export { pinned };
export const options = createState({ persist: true } as any);
export function make() { return createEvent('dynamic'); }
"#;
        let url = deno_core::ModuleSpecifier::parse("file:///index.ts").unwrap();

        // 1. Extraction: every top-level declaration, names by the shared rule.
        let extraction = extract_interop_handles(&url, FIXTURE).expect("fixture parses");
        let summary: Vec<(&str, InteropKind, bool)> = extraction
            .handles
            .iter()
            .map(|h| (h.name.as_str(), h.kind, h.exported))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("vitals", InteropKind::State, true),
                ("prompt", InteropKind::Event, true),
                ("refresh", InteropKind::Procedure, true),
                ("hpPct", InteropKind::State, true),
                // `export { pinned }` (spelling == identity fold) is a real export of the
                // declaration: typings derive its payload from `typeof import(entry).pinned`.
                ("Pinned", InteropKind::State, true),
                ("options", InteropKind::State, true),
            ],
        );
        assert!(extraction.duplicates.is_empty());
        // `export { pinned }` spells the identity's fold exactly — not a diagnostic.
        assert!(extraction.export_diagnostics.is_empty(), "{:#?}", extraction.export_diagnostics);
        assert_eq!(
            extraction.handles[0].doc.as_deref(),
            Some("/** The current vitals reading. */")
        );
        assert_eq!(
            extraction.handles[0].payload_type_export.as_deref(),
            Some("VitalData")
        );

        // 2. Injection: names spliced in; explicit names + nested creation untouched; and —
        // the agreement property — extraction over the injected source sees the same handles.
        let injected = inject_inferred_handle_names(&url, FIXTURE).expect("injects");
        assert!(injected.contains(r#"createState<VitalData>("vitals")"#), "{injected}");
        assert!(injected.contains(r#"createEvent<{ raw: string }>("prompt")"#), "{injected}");
        assert!(injected.contains(r#"createProcedure("refresh", (args"#), "{injected}");
        assert!(injected.contains(r#"createDerived("hpPct", vitals as any"#), "{injected}");
        assert!(injected.contains(r#"createState("options", { persist: true } as any)"#), "{injected}");
        assert!(injected.contains("createState<VitalData>('Pinned')"), "{injected}");
        assert!(injected.contains("createEvent('dynamic')"), "{injected}");
        assert_eq!(FIXTURE.lines().count(), injected.lines().count());
        let re_extracted = extract_interop_handles(&url, &injected).expect("injected parses");
        let re_summary: Vec<(&str, InteropKind, bool)> = re_extracted
            .handles
            .iter()
            .map(|h| (h.name.as_str(), h.kind, h.exported))
            .collect();
        assert_eq!(summary, re_summary, "injection must not change what extraction sees");

        // 3. Scrub: every exported handle's export-ness removed (the aliased `pinned` named
        // export too), nothing else touched, line count preserved.
        let (scrubbed, removed) = scrub_handle_exports(&url, FIXTURE).expect("scrubs");
        assert_eq!(removed, vec!["vitals", "prompt", "refresh", "hpPct", "pinned", "options"]);
        assert!(scrubbed.contains("export interface VitalData"), "{scrubbed}");
        assert!(scrubbed.contains("export function make()"), "{scrubbed}");
        assert!(!scrubbed.contains("export const vitals"), "{scrubbed}");
        assert!(!scrubbed.contains("export { pinned }"), "{scrubbed}");
        assert_eq!(FIXTURE.lines().count(), scrubbed.lines().count());

        // 4. Typings: every extracted handle appears in its kind's scheme module with a twin.
        let packages = [InstalledPackageTypes {
            owner: "wbk".to_string(),
            name: "fixture".to_string(),
            entry_module: "index.ts".to_string(),
            handles: extraction.handles.clone(),
            local: false,
        }];
        let shims = interop_handles_dts(&packages);
        for (name, scheme) in [
            ("vitals", "state"),
            ("hpPct", "state"),
            ("Pinned", "state"),
            ("options", "state"),
            ("prompt", "events"),
            ("refresh", "procedures"),
        ] {
            assert!(
                shims.contains(&format!("declare module \"smudgy:{scheme}/wbk/fixture/{name}\"")),
                "missing subpath module for {name}:\n{shims}"
            );
            assert!(
                shims.contains(&format!("export type {name} = ")),
                "missing twin type for {name}:\n{shims}"
            );
        }
        assert!(
            shims.contains("ConsumerOf<typeof import(\"smudgy://wbk/fixture\").vitals>"),
            "{shims}"
        );
        assert!(
            shims.contains("export type { VitalData } from \"smudgy://wbk/fixture\";"),
            "{shims}"
        );
        assert!(shims.contains("/** The current vitals reading. */"), "{shims}");
    }

    /// Compile the *real* shipped `smudgy-core.d.ts` (not a test mirror) through the
    /// publish-time generator as the ambient, with a consumer that exercises the interop
    /// handle surface end to end: producer `createState()`/`createEvent()` declaration with the erased
    /// `typeof` alias pattern, `ConsumerOf` payload derivation (the mechanism the generated
    /// `interop-handles.d.ts` shims rely on), the typed platform catalogs
    /// (`smudgy:events/sys` / `smudgy:events/map`), the `events.lookup` escape hatch, and
    /// the `EventSubscription` each subscription returns (`.off()`). A clean compile proves
    /// the actual surface authors see is internally consistent.
    #[test]
    fn real_smudgy_core_dts_types_interop_end_to_end() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        // smudgy-core.d.ts's `mapper` member references the global `Mapper` declared in the
        // sibling mapper typings, so the ambient set must include it (as it does in the editor
        // project and at publish time).
        ambient.insert("smudgy-mapper.d.ts".to_string(), SMUDGY_MAPPER_DTS.to_string());

        let mut sources = BTreeMap::new();
        sources.insert(
            "producer.ts".to_string(),
            "import { createState, createEvent, createProcedure } from \"smudgy:core\";\n\
             import type { Binding } from \"smudgy:core\";\n\
             export interface PromptData { hp: number; maxhp: number }\n\
             const promptState = createState<PromptData>('promptState');\n\
             const prompt = createEvent<PromptData>('prompt');\n\
             const refreshRequest = createProcedure('refreshRequest', (payload: { full: boolean }, sender) => { const f: boolean = payload.full; void f; const who: string = sender; void who; });\n\
             export type PromptState = typeof promptState;\n\
             export type PromptEvent = typeof prompt;\n\
             export type RefreshRequest = typeof refreshRequest;\n\
             export function publish() {\n\
               promptState.set({ hp: 42, maxhp: 100 });\n\
               promptState.set(\"hp\", 43);\n\
               // The mutation proxy (plan 4a): assignments type as T members.\n\
               promptState.value.hp = 44;\n\
               const viaProxy: number = promptState.value.maxhp; void viaProxy;\n\
               // previousValue (plan 5): the read-only pre-batch view on the producer seat.\n\
               const mine: number | undefined = promptState.previousValue?.hp; void mine;\n\
               prompt.emit({ hp: 43, maxhp: 100 });\n\
               // Widget bindings (plan 7): typed paths derive the bound value's type; the\n\
               // whole-value form carries T; bracket/computed paths fall back to Binding<any>.\n\
               const hp: Binding<number> = promptState.bind('hp', { fallback: 0, format: '{}%' });\n\
               void hp;\n\
               const whole: Binding<PromptData> = promptState.bind(); void whole;\n\
               const dynamic = promptState.bind('groupies[\"Mr. Foo\"].hp'); void dynamic;\n\
             }\n"
                .to_string(),
        );
        sources.insert(
            "consumer.ts".to_string(),
            "import { events, session, mapper, getSessions, createHotkey, line, createDerived } from \"smudgy:core\";\n\
             import type { ConsumerOf, Payload } from \"smudgy:core\";\n\
             import { connect, send } from \"smudgy:events/sys\";\n\
             import { room } from \"smudgy:events/map\";\n\
             import type { PromptState, PromptEvent, RefreshRequest } from \"./producer.ts\";\n\
             // ConsumerOf derives the consumer surface from the producer's erased aliases —\n\
             // exactly what the generated smudgy:state/… shims do.\n\
             declare const promptState: ConsumerOf<PromptState>;\n\
             declare const prompt: ConsumerOf<PromptEvent>;\n\
             declare const refreshRequest: ConsumerOf<RefreshRequest>;\n\
             // The Payload helper names what handlers receive, from either seat.\n\
             function onPrompt(p: Payload<typeof prompt>) { const v: number = p.hp; void v; }\n\
             function onAsk(a: Payload<RefreshRequest>) { const f: boolean = a.full; void f; }\n\
             export function wire() {\n\
               prompt.on(onPrompt).off();\n\
               void onAsk;\n\
               // The consumer's pre-batch view types like the live one (plan 5).\n\
               const hp: number | undefined = promptState.previousValue?.hp; void hp;\n\
               // The consumer's read-only live view: undefined until the producer\n\
               // publishes, then leaf reads type as T members.\n\
               const leaf: number | undefined = promptState.value?.hp; void leaf;\n\
               promptState.watch((next) => { const m: number | undefined = next?.maxhp; void m; }).off();\n\
               // The per-write cadence: every write, in order, with the written path.\n\
               promptState.onWrite((path, snapshot) => { const p: string = path; void p; void snapshot; }).off();\n\
               // Directed procedures: the consumer seat posts; sender stamping is host-side.\n\
               refreshRequest.post({ full: true });\n\
               // Consumer-side derivation (plan 4b): computed over state you do not own,\n\
               // published as your own - hence bindable like any state.\n\
               const hpPct = createDerived('hpPct', promptState, (v) => v.hp / v.maxhp);\n\
               const pctBind: import(\"smudgy:core\").Binding<number> = hpPct.bind(); void pctBind;\n\
               const pct: number | undefined = hpPct.value; void pct;\n\
               hpPct.off();\n\
               // The consumer seat binds too (read-side, like watch).\n\
               const bound: import(\"smudgy:core\").Binding<number> = promptState.bind('maxhp');\n\
               void bound;\n\
               prompt.on((p) => { const v: number = p.hp; void v; });\n\
               prompt.once((p) => { void p.maxhp; });\n\
               const sub = room.on((p) => { const a: string = p.areaId; void a; void p.roomNumber; });\n\
               sub.off();\n\
               send.on((p) => { const c: string = p.command; void c; });\n\
               connect.once(() => {}).off();\n\
               events.lookup(\"smudgy://o/n\", \"anything-dynamic\").on((p) => { void p; });\n\
               // The non-event surface: named session/mapper exports, getSessions(), the\n\
               // createHotkey signature, a typed Line, and the mapper's AreaId pair.\n\
               const sid: number = session.id; void sid;\n\
               for (const s of getSessions()) { s.send(\"x\"); }\n\
               createHotkey({ key: \"F1\", modifiers: [\"ctrl\"] }, () => {}).delete();\n\
               const t: string = line.text; void t;\n\
               const a = mapper.areas[0];\n\
               if (a) { const r = a.room(1); void r; const id: readonly [number, number] = a.id; void id; }\n\
             }\n"
                .to_string(),
        );

        // The GMCP page's cast example (scriptref:gmcp): a game that reports room
        // vnums under Room.Info.id instead of Room.Info.num narrows the handle to
        // its own tree shape. Kept identical to the published snippet so the docs
        // stay compile-checked.
        sources.insert(
            "gmcp_cast.ts".to_string(),
            "import gmcp from \"smudgy:state/gmcp\";\n\
             import type { StateConsumer, GmcpTree } from \"smudgy:core\";\n\
             interface FenworldGmcp extends GmcpTree {\n\
               Room?: {\n\
                 Info?: { id?: number; name?: string; [field: string]: unknown };\n\
                 [message: string]: unknown;\n\
               };\n\
             }\n\
             const fenGmcp = gmcp as StateConsumer<FenworldGmcp>;\n\
             export function readVnum(): number | undefined {\n\
               const vnum: number | undefined = fenGmcp.value?.Room?.Info?.id;\n\
               const nm: string | undefined = fenGmcp.value?.Room?.Info?.name;\n\
               void nm;\n\
               return vnum;\n\
             }\n\
             // The GmcpTree doc's addition example: a new message under a declared\n\
             // package, intersected so the declared Room.Info typing survives.\n\
             interface WeatherGmcp extends GmcpTree {\n\
               Room?: NonNullable<GmcpTree['Room']> & {\n\
                 Weather?: { temp?: number; rain?: boolean };\n\
               };\n\
             }\n\
             const wxGmcp = gmcp as StateConsumer<WeatherGmcp>;\n\
             export function readWeather(): number | undefined {\n\
               const temp: number | undefined = wxGmcp.value?.Room?.Weather?.temp;\n\
               const kept: string | undefined = wxGmcp.value?.Room?.Info?.name;\n\
               void kept;\n\
               return temp;\n\
             }\n"
                .to_string(),
        );

        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("generate against the real smudgy-core.d.ts");
        assert!(
            out.diagnostics.is_empty(),
            "the shipped smudgy-core.d.ts produced diagnostics: {:?}",
            out.diagnostics
        );
    }

    /// The consumer seat must not carry producer verbs (interop.md §4c): assigning a consumer
    /// handle's surface where the producer's is expected — or calling `.emit` / `.set` on a
    /// consumer — must fail to compile.
    #[test]
    fn consumer_handles_lack_producer_verbs() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert("smudgy-mapper.d.ts".to_string(), SMUDGY_MAPPER_DTS.to_string());

        let mut sources = BTreeMap::new();
        sources.insert(
            "consumer.ts".to_string(),
            "import type { StateConsumer, EventConsumer, ProcedureConsumer } from \"smudgy:core\";\n\
             declare const s: StateConsumer<{ hp: number }>;\n\
             declare const e: EventConsumer<{ hp: number }>;\n\
             declare const m: ProcedureConsumer<{ full: boolean }>;\n\
             s.set({ hp: 1 });\n\
             e.emit({ hp: 1 });\n\
             m.on((payload: unknown) => { void payload; });\n"
                .to_string(),
        );
        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("the generator itself must not crash on a type error");
        assert!(
            !out.diagnostics.is_empty(),
            "producer verbs on consumer handles must be compile errors"
        );

        // The consumer's `.value` is a read-only view: assignment through it must fail to
        // compile — checked on its own, so the verb errors above can't mask a regression.
        // The non-null assertion isolates the read-only error from the absent-until-published
        // (`| undefined`) one.
        let mut sources = BTreeMap::new();
        sources.insert(
            "consumer_value.ts".to_string(),
            "import type { StateConsumer } from \"smudgy:core\";\n\
             declare const s: StateConsumer<{ hp: number }>;\n\
             s.value!.hp = 2;\n"
                .to_string(),
        );
        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("the generator itself must not crash on a type error");
        assert!(
            !out.diagnostics.is_empty(),
            "assignment through the consumer's read-only .value must be a compile error"
        );

        // `previousValue` is read-only on BOTH seats (interop-pre-gmcp-plan.md §5): even on
        // the producer handle, whose `.value` writes, assignment through the pre-batch view
        // must fail to compile.
        let mut sources = BTreeMap::new();
        sources.insert(
            "producer_previous.ts".to_string(),
            "import type { StateHandle } from \"smudgy:core\";\n\
             declare const s: StateHandle<{ hp: number }>;\n\
             s.previousValue!.hp = 2;\n"
                .to_string(),
        );
        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("the generator itself must not crash on a type error");
        assert!(
            !out.diagnostics.is_empty(),
            "assignment through the producer's read-only .previousValue must be a compile error"
        );
    }

    /// Drift guard for the platform catalogs: every event the runtime synthesis exports
    /// (`platform_event_catalog` in the script crate) must be declared in the corresponding
    /// `declare module "smudgy:events/…"` block of the shipped contract, and vice versa the
    /// declared modules must exist. (Payload shapes are exercised by the end-to-end test.)
    #[test]
    fn platform_event_modules_match_runtime_synthesis() {
        // The platform STATE producers ship typed modules too (a single root handle,
        // synthesized specially — presence is the drift axis, not an export list).
        for producer in ["gmcp", "msdp"] {
            assert!(
                smudgy_script::platform_state_producer(producer),
                "{producer} is a platform state producer"
            );
            assert!(
                SMUDGY_CORE_DTS.contains(&format!("declare module \"smudgy:state/{producer}\"")),
                "smudgy:state/{producer} missing from smudgy-core.d.ts"
            );
        }
        for producer in ["sys", "map", "gmcp", "msdp"] {
            let catalog = smudgy_script::platform_event_catalog(producer);
            assert!(!catalog.is_empty(), "platform catalog {producer} is empty");
            let header = format!("declare module \"smudgy:events/{producer}\"");
            let start = SMUDGY_CORE_DTS
                .find(&header)
                .unwrap_or_else(|| panic!("{header} missing from smudgy-core.d.ts"));
            let block_end = SMUDGY_CORE_DTS[start..]
                .find("\ndeclare module")
                .map_or(SMUDGY_CORE_DTS.len(), |o| start + o + 1);
            let block = &SMUDGY_CORE_DTS[start..block_end];
            for name in catalog {
                assert!(
                    block.contains(&format!("export const {name}:")),
                    "smudgy:events/{producer} declaration is missing `{name}` (runtime synthesizes it)"
                );
            }
        }
    }

    /// The Phase-5 typed split spec (`PaneSpec<D>`): the initial size is keyed to the split
    /// axis, so `width` on a `left`/`right` split (and `height` on `top`/`bottom`) compiles,
    /// while the off-axis dimension is a compile error (`never` on the wrong key). Also
    /// exercises `titleBar` round-tripping through the contract.
    #[test]
    fn pane_spec_keys_size_to_the_split_axis() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert("smudgy-mapper.d.ts".to_string(), SMUDGY_MAPPER_DTS.to_string());

        let good = "import { session } from \"smudgy:core\";\n\
             import type { TitleBarSpec } from \"smudgy:core\";\n\
             export function wire() {\n\
               const pinned: TitleBarSpec = \"always-show\";\n\
               const chat = session.mainPane.split(\"right\", { name: \"chat\", width: 300, titleBar: pinned });\n\
               chat.split(\"bottom\", { name: \"log\", height: 120, terminal: false, titleBar: \"normal\" });\n\
               session.mainPane.split(\"top\", { name: \"status\", height: 80 });\n\
               session.mainPane.split(\"left\", { name: \"map\" });\n\
             }\n";
        let mut sources = BTreeMap::new();
        sources.insert("consumer.ts".to_string(), good.to_string());
        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("generate the axis-correct pane-spec consumer");
        assert!(
            out.diagnostics.is_empty(),
            "axis-correct split specs must compile cleanly: {:?}",
            out.diagnostics
        );

        // The negative half: `height` on a horizontal (`right`) split and `width` on a
        // vertical (`bottom`) split must each fail to compile.
        let bad = "import { session } from \"smudgy:core\";\n\
             export function wire() {\n\
               session.mainPane.split(\"right\", { name: \"chat\", height: 300 });\n\
               session.mainPane.split(\"bottom\", { name: \"log\", width: 120 });\n\
             }\n";
        let mut sources = BTreeMap::new();
        sources.insert("consumer.ts".to_string(), bad.to_string());
        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("the generator itself must not crash on a type error");
        assert!(
            !out.diagnostics.is_empty(),
            "an off-axis size key must be a compile error, but the consumer compiled cleanly"
        );
    }

    /// Compile a real `.tsx` widget module against the shipped widgets typings through the
    /// (jsx-aware) publish-time generator. A clean compile proves the `smudgy:widgets` module
    /// surface, the `smudgy:widgets/jsx-runtime` automatic runtime, and the `JSX` namespace are
    /// internally consistent — i.e. that `<Column/>`-style authoring type-checks against the
    /// component prop shapes and that no host string tags leak in (empty `IntrinsicElements`).
    #[test]
    fn real_smudgy_widgets_dts_types_a_tsx_consumer() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert("smudgy-mapper.d.ts".to_string(), SMUDGY_MAPPER_DTS.to_string());
        ambient.insert("smudgy-widgets.d.ts".to_string(), SMUDGY_WIDGETS_DTS.to_string());

        let mut sources = BTreeMap::new();
        sources.insert(
            "ui.tsx".to_string(),
            "import { createWidget, Column, Row, Text, ProgressBar, Button, MapView } from \"smudgy:widgets\";\n\
             import { session, createState } from \"smudgy:core\";\n\
             interface Vitals { hp: number; maxhp: number; name: string }\n\
             const vitals = createState<Vitals>('vitals');\n\
             const untyped = createState('untyped');\n\
             export function mount() {\n\
               const panel = (\n\
                 <Column spacing={4} width=\"fill\">\n\
                   <Text color=\"red\" size={18}>Hello</Text>\n\
                   <Row spacing={2}>\n\
                     <ProgressBar min={0} max={100} value={42} vertical={false} />\n\
                     <Button onPress={() => {}}>Click</Button>\n\
                   </Row>\n\
                   {false && <Text>conditional</Text>}\n\
                   <MapView />\n\
                 </Column>\n\
               );\n\
               createWidget(\"panel\", panel);\n\
               // The pane option in both accepted forms: a pane name and a Pane handle.\n\
               createWidget(\"docked\", panel, { pane: \"chat\" });\n\
               createWidget(\"hud\", panel, { pane: session.mainPane });\n\
               // Store bindings at prop positions and as mixed Text children (plan 7):\n\
               // typed paths type-check against the prop, an untyped handle's Binding<any>\n\
               // is accepted anywhere, and format/fallback ride the token.\n\
               createWidget(\"bound\",\n\
                 <Column spacing={vitals.bind('hp')}>\n\
                   <ProgressBar value={vitals.bind('hp')} max={vitals.bind('maxhp')} color={vitals.bind('name')} />\n\
                   <ProgressBar value={untyped.bind('anything.at.all')} />\n\
                   <Text size={vitals.bind('hp')}>HP: {vitals.bind('hp', { fallback: 0, format: \"{}%\" })}/{vitals.bind('maxhp')}</Text>\n\
                   <Button width={vitals.bind('hp')}>{vitals.bind('name')}</Button>\n\
                 </Column>,\n\
               );\n\
             }\n"
                .to_string(),
        );
        // A second consumer exercises Scrollable + Markdown + Modal + TextEditor + the Button
        // `variant` (incl. the onLink/onDismiss/onChange callback shapes) so a clean compile proves
        // them too -- this mirrors the notes-editor modal shape.
        sources.insert(
            "doc.tsx".to_string(),
            "import { createWidget, Scrollable, Markdown, Modal, TextEditor, Button } from \"smudgy:widgets\";\n\
             export function mountDoc() {\n\
               let draft = \"\";\n\
               createWidget(\"doc\",\n\
                 <Modal onDismiss={() => {}} background=\"rgba(0,0,0,0.6)\">\n\
                   <Scrollable height=\"fill\" direction=\"vertical\" anchor=\"end\">\n\
                     <Markdown size={14} onLink={(url) => { void url; }}>Hello world</Markdown>\n\
                     <TextEditor id=\"notes\" value={draft} height={200} onChange={(t) => { draft = t; }} />\n\
                     <Button variant=\"primary\" onPress={() => { void draft; }}>Save</Button>\n\
                   </Scrollable>\n\
                 </Modal>,\n\
               );\n\
             }\n"
                .to_string(),
        );

        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("generate against the real smudgy-widgets.d.ts");
        assert!(
            out.diagnostics.is_empty(),
            "the shipped smudgy-widgets.d.ts produced diagnostics on a .tsx consumer: {:?}",
            out.diagnostics
        );
        assert!(
            out.files.contains_key("ui.d.ts"),
            "the .tsx module must emit a .d.ts; got {:?}",
            out.files.keys().collect::<Vec<_>>()
        );
    }

    /// The strict half of binding prop types: a `Binding<string>` (a typed path to a string
    /// field) offered to a numeric prop must fail to compile — `Bindable<number>` admits
    /// `Binding<number>` and the untyped `Binding<any>`, not a known-wrong payload.
    #[test]
    fn mistyped_binding_props_fail_to_compile() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert("smudgy-mapper.d.ts".to_string(), SMUDGY_MAPPER_DTS.to_string());
        ambient.insert("smudgy-widgets.d.ts".to_string(), SMUDGY_WIDGETS_DTS.to_string());

        let mut sources = BTreeMap::new();
        sources.insert(
            "ui.tsx".to_string(),
            "import { ProgressBar } from \"smudgy:widgets\";\n\
             import { createState } from \"smudgy:core\";\n\
             const vitals = createState<{ hp: number; name: string }>('vitals');\n\
             export const bad = <ProgressBar value={vitals.bind('name')} />;\n"
                .to_string(),
        );
        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("the generator itself must not crash on a type error");
        assert!(
            !out.diagnostics.is_empty(),
            "a Binding<string> on a numeric prop must be a compile error"
        );
    }

    /// The runtime implementation in `js/smudgy.ts`, type-checked against the contract here.
    /// deno's extension transpiler only type-STRIPS this file at runtime, so this is the only
    /// place its TypeScript is actually checked.
    const SMUDGY_TS: &str = include_str!("../session/runtime/js/smudgy.ts");

    /// The mapper runtime implementation (`script_engine/mapper/mapper.ts`), type-checked
    /// against the `smudgy-mapper.d.ts` contract below. Like `smudgy.ts`, deno's extension
    /// transpiler only type-STRIPS it at runtime, so this is the only TypeScript check it gets.
    const SMUDGY_MAPPER_TS: &str =
        include_str!("../session/runtime/script_engine/mapper/mapper.ts");

    /// Drift guard: the runtime impl (`smudgy.ts`) and the author-facing contract
    /// (`smudgy-core.d.ts`) are separate files, so this compiles them together and asserts
    /// (1) the impl is valid TypeScript on its own (ops are an `any` FFI boundary via the
    /// `@ts-ignore`d `ext:core/ops` import) and (2) the api object the impl builds is assignable
    /// to the published `SmudgyApi` interface — so the impl cannot silently expose less than, or
    /// a type incompatible with, what the declarations promise authors.
    #[test]
    fn smudgy_ts_impl_conforms_to_contract() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert("smudgy-mapper.d.ts".to_string(), SMUDGY_MAPPER_DTS.to_string());

        let mut sources = BTreeMap::new();
        sources.insert("impl.ts".to_string(), SMUDGY_TS.to_string());
        sources.insert(
            "check.ts".to_string(),
            "import type { SmudgyApi } from \"smudgy:core\";\n\
             import type { SmudgyCoreApi } from \"./impl.ts\";\n\
             declare const __impl: SmudgyCoreApi;\n\
             // The impl must fulfill the published contract.\n\
             export const __conforms: SmudgyApi = __impl;\n"
                .to_string(),
        );

        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("compile the smudgy.ts impl against the contract");
        assert!(
            out.diagnostics.is_empty(),
            "smudgy.ts impl does not type-check / conform to smudgy-core.d.ts:\n{:#?}",
            out.diagnostics
        );
    }

    /// The name-first deprecation shim (the `DEPRECATED-NAME-FIRST` section of `smudgy.ts`)
    /// honors pre-0.4 `create*(name, ...)` calls with a notice through the 0.4 line only —
    /// the contract never carried the old form, so 0.5 is where the runtime stops accepting
    /// it. This trips the moment the crate version reaches 0.5 while the shim still exists,
    /// so the removal cannot be forgotten in the release rush.
    #[test]
    fn name_first_shim_is_removed_by_0_5() {
        let mut parts = env!("CARGO_PKG_VERSION").split(['.', '-']);
        let major: u32 = parts.next().and_then(|p| p.parse().ok()).expect("major");
        let minor: u32 = parts.next().and_then(|p| p.parse().ok()).expect("minor");
        assert!(
            (major, minor) < (0, 5) || !SMUDGY_TS.contains("DEPRECATED-NAME-FIRST"),
            "smudgy is {} but smudgy.ts still contains the DEPRECATED-NAME-FIRST shim: the \
             name-first create* grace window ended at 0.5 — delete the shim section, the \
             rest-args facade wrappers, and the createTimer/createHotkey entry shims, and \
             flip the old-form tests in script_integration.rs to expect a TypeError.",
            env!("CARGO_PKG_VERSION"),
        );
    }

    /// Drift guard for the MAP types: the mapper runtime impl (`mapper.ts`) and the
    /// author-facing contract (`smudgy-mapper.d.ts`) are separate files, so this compiles them
    /// together and asserts (1) the impl is valid TypeScript on its own (ops are an `any` FFI
    /// boundary via the `@ts-ignore`d `ext:core/ops` import) and (2) the impl's `mapper` object
    /// and `Area`/`Room`/`Exit` shapes are assignable to the published global `Mapper`/`Area`/
    /// `Room`/`Exit` — so the runtime cannot silently expose map types incompatible with what the
    /// declarations promise authors (the regression that left external packages' `Room`/`Area`/…
    /// usage stranded). The impl exposes these via the type-only `*Impl` exports at the end of
    /// `mapper.ts`.
    #[test]
    fn mapper_ts_impl_conforms_to_contract() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        // The contract declares `Mapper`/`Area`/`Room`/`Exit`/`AreaId`/… as global ambient types.
        ambient.insert("smudgy-mapper.d.ts".to_string(), SMUDGY_MAPPER_DTS.to_string());

        let mut sources = BTreeMap::new();
        sources.insert("impl.ts".to_string(), SMUDGY_MAPPER_TS.to_string());
        sources.insert(
            "check.ts".to_string(),
            "import type { MapperImpl, AreaImpl, RoomImpl, ExitImpl } from \"./impl.ts\";\n\
             declare const m: MapperImpl;\n\
             declare const a: AreaImpl;\n\
             declare const r: RoomImpl;\n\
             declare const e: ExitImpl;\n\
             // The runtime impl must fulfill the published global map-type contract.\n\
             export const __mapper: Mapper = m;\n\
             export const __area: Area = a;\n\
             export const __room: Room = r;\n\
             export const __exit: Exit = e;\n"
                .to_string(),
        );

        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("compile the mapper.ts impl against the contract");
        assert!(
            out.diagnostics.is_empty(),
            "mapper.ts impl does not type-check / conform to smudgy-mapper.d.ts:\n{:#?}",
            out.diagnostics
        );
    }

    /// Rust↔TS enum drift guard: every serde variant name of the map enums must appear
    /// (quoted) in BOTH the published contract and the runtime impl, whose string unions
    /// mirror the Rust enums by hand. The impl↔contract guards above cannot catch this class
    /// (they compare TS against TS); this pins both to the Rust source of truth — the
    /// regression that added a fifth `ExitStyle` (`Stub`) invisible to scripts. A tripwire on
    /// quoted-name presence, not a union parser: adding a Rust variant fails here until the
    /// unions name it.
    #[test]
    fn map_enum_unions_cover_rust_variants() {
        fn assert_covered<T: serde::Serialize>(variants: &[T], enum_name: &str) {
            for variant in variants {
                let quoted =
                    serde_json::to_string(variant).expect("serialize a plain enum variant");
                for (file, body) in [
                    ("smudgy-mapper.d.ts", SMUDGY_MAPPER_DTS),
                    ("mapper.ts", SMUDGY_MAPPER_TS),
                ] {
                    assert!(
                        body.contains(&quoted),
                        "{enum_name} variant {quoted} is missing from {file} — update its string union"
                    );
                }
            }
        }
        assert_covered(&smudgy_cloud::ExitStyle::ALL, "ExitStyle");
        assert_covered(&smudgy_cloud::ExitDirection::ALL, "ExitDirection");
        assert_covered(&smudgy_cloud::ShapeType::ALL, "ShapeType");
        assert_covered(&smudgy_cloud::HorizontalAlignment::ALL, "HorizontalAlignment");
        assert_covered(&smudgy_cloud::VerticalAlignment::ALL, "VerticalAlignment");
    }

    /// Coverage guard for EXTERNAL packages: compile a consumer that reaches the map the way
    /// installed `smudgy://` package scripts do — `Room`/`Area`/`Exit`/`ExitId`/`RoomNumber`/
    /// `AreaId` as AMBIENT GLOBALS (no import) and the bare `mapper` global — against the shipped
    /// typings. This is the surface already-published scripts compile against, so a typings/runtime
    /// refactor that drops one of these globals silently breaks them. A clean compile proves the map
    /// types stay exposed to `smudgy:core` consumers; a regression here is the "map types no longer
    /// available" breakage.
    #[test]
    fn external_package_map_surface_is_typed() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert("smudgy-mapper.d.ts".to_string(), SMUDGY_MAPPER_DTS.to_string());

        let mut sources = BTreeMap::new();
        sources.insert(
            "consumer.ts".to_string(),
            // `mapper` and Room/Area/Exit/ExitId/RoomNumber/AreaId are used with NO import,
            // resolving to the global ambient declarations -- the way package scripts reach the map.
            r##"
            function useRoom(room: Room): void {
              const aid: AreaId = room.area_id;
              const n: RoomNumber = room.room_number;
              const t: string = room.title;
              const d: string = room.description;
              const x: number = room.x; const y: number = room.y; const l: number = room.level;
              const c: string = room.color;
              const exits: Exit[] = room.exits;
              const tags: string[] = room.tags;
              const has: boolean = room.hasTag("INN");
              const notes: string | undefined = room.data("notes");
              void aid; void n; void t; void d; void x; void y; void l; void c; void exits; void tags; void has; void notes;
            }
            function useArea(area: Area): void {
              const id: AreaId = area.id;
              const name: string = area.name;
              const nums: RoomNumber[] = area.room_numbers;
              const next: RoomNumber = area.next_room_number;
              const r: Room | undefined = area.room(1);
              const p: string | undefined = area.data("notes");
              void id; void name; void nums; void next; void r; void p;
            }
            function useExit(e: Exit): void {
              const id: ExitId = e.id;
              const fd = e.from_direction;
              const fa: AreaId = e.from_area_id;
              const fr: RoomNumber = e.from_room_number;
              const ta = e.to_area_id; const tr = e.to_room_number; const td = e.to_direction;
              const closed: boolean = e.is_closed; const hidden: boolean = e.is_hidden; const locked: boolean = e.is_locked;
              const w: number = e.weight; const cmd = e.command;
              void id; void fd; void fa; void fr; void ta; void tr; void td; void closed; void hidden; void locked; void w; void cmd;
            }
            // `mapper` as a bare ambient global (package scripts reference it without importing).
            async function useMapper(room: Room): Promise<void> {
              const areas: Area[] = mapper.areas;
              const a: Area = mapper.getAreaById(room.area_id);
              const path: [AreaId, RoomNumber][] = mapper.getPathBetweenRooms(room.area_id, room.room_number, room.area_id, room.room_number);
              const near: Room | undefined = mapper.findNearestRoomWithTags(room, { all: ["INN"], none: ["PEACE"] });
              const near1: Room | undefined = mapper.findNearestRoomWithTag(room, "INN");
              const near2: Room | undefined = mapper.findNearestRoomInArea(room, room.area_id);
              const near3: Room | undefined = mapper.findNearestRoomInArea(room, a);
              const list = mapper.listRoomsByTitleAndDescription("t", "d");
              const list2 = mapper.listRoomsByTitleDescriptionAndVisibleExits("t", "d", ["North"]);
              const newArea: Area = await mapper.createArea("Town");
              const newRoom: RoomNumber = mapper.createRoom(room.area_id, { title: "x" });
              const exitId: ExitId = await mapper.createRoomExit(room.area_id, room.room_number, { from_direction: "North" });
              mapper.setRoomExit(room.area_id, room.room_number, exitId, { command: "enter hole" });
              mapper.deleteRoomExit(room.area_id, room.room_number, exitId);
              mapper.deleteRoom(room.area_id, room.room_number);
              mapper.setCurrentLocation(room.area_id, room.room_number);
              mapper.setRoomProperty(room.area_id, room.room_number, "k", "v");
              mapper.setAreaProperty(room.area_id, "k", "v");
              mapper.addRoomTag(room.area_id, room.room_number, "INN");
              mapper.removeRoomTag(room.area_id, room.room_number, "INN");
              mapper.setRoomColor(room.area_id, room.room_number, "#fff");
              mapper.setRoomX(room.area_id, room.room_number, 1);
              mapper.setRoomY(room.area_id, room.room_number, 1);
              mapper.setRoomLevel(room.area_id, room.room_number, 1);
              mapper.setRoomTitle(room.area_id, room.room_number, "t");
              mapper.setRoomDescription(room.area_id, room.room_number, "d");
              mapper.renameArea(room.area_id, "n");
              void areas; void a; void path; void near; void near1; void list; void list2; void newArea; void newRoom;
            }
            export { useRoom, useArea, useExit, useMapper };
            "##
            .to_string(),
        );

        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("compile the external map-surface consumer against the shipped typings");
        assert!(
            out.diagnostics.is_empty(),
            "the shipped typings no longer expose the external map surface:\n{:#?}",
            out.diagnostics
        );
    }

    #[test]
    fn writes_deno_and_node_runtime_typings() {
        let dir = temp_server_dir("runtime-types");
        ensure_script_tsconfig_in(&dir, &[]).expect("ensure");

        // Deno lib (Deno namespace) + @types/node materialized to disk.
        assert!(dir.join(".smudgy/types/deno/lib.deno.ns.d.ts").is_file());
        assert!(dir.join(".smudgy/node-types/@types/node/events.d.ts").is_file());
        assert!(dir.join(".smudgy/.runtime-types-version").is_file());

        // The base tsconfig wires @types/node via types + typeRoots.
        let base = fs::read_to_string(dir.join(".smudgy/tsconfig.base.json")).unwrap();
        assert!(base.contains("\"node\""), "types: [node] missing:\n{base}");
        assert!(base.contains("./node-types/@types"), "typeRoots missing:\n{base}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn is_idempotent_across_runs() {
        let dir = temp_server_dir("idem");
        ensure_script_tsconfig_in(&dir, &[]).expect("first run");
        ensure_script_tsconfig_in(&dir, &[]).expect("second run");

        assert!(dir.join(".smudgy/tsconfig.base.json").is_file());
        assert!(dir.join(".smudgy/types/smudgy-core.d.ts").is_file());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrates_stale_modules_level_project() {
        let dir = temp_server_dir("migrate");
        let modules = dir.join("modules");
        fs::create_dir_all(modules.join(".smudgy/types")).unwrap();
        // A smudgy-generated modules-level tsconfig (carries the marker) is removed…
        fs::write(modules.join("tsconfig.json"), TSCONFIG_USER).unwrap();

        ensure_script_tsconfig_in(&dir, &[]).expect("ensure");

        assert!(!modules.join(".smudgy").exists(), "stale managed dir removed");
        // The stale heavy generated stub is replaced by the thin pointer at the server-level project
        // (it carries the new `../tsconfig.json` extends, not the old base-config marker).
        let modules_ts = fs::read_to_string(modules.join("tsconfig.json"))
            .expect("modules tsconfig seeded after migration");
        assert!(modules_ts.contains("../tsconfig.json"), "thin pointer expected:\n{modules_ts}");
        assert!(
            !modules_ts.contains(USER_TSCONFIG_MARKER),
            "stale base-config stub should be gone:\n{modules_ts}"
        );
        // …and the project now lives at the server dir.
        assert!(dir.join("tsconfig.json").is_file());
        assert!(dir.join(".smudgy/tsconfig.base.json").is_file());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migration_leaves_author_written_modules_tsconfig_alone() {
        let dir = temp_server_dir("migrate-keep");
        let modules = dir.join("modules");
        fs::create_dir_all(&modules).unwrap();
        let authored = "{ \"compilerOptions\": { \"strict\": false } }"; // no smudgy marker
        fs::write(modules.join("tsconfig.json"), authored).unwrap();

        ensure_script_tsconfig_in(&dir, &[]).expect("ensure");

        assert_eq!(
            fs::read_to_string(modules.join("tsconfig.json")).unwrap(),
            authored,
            "an author's own modules/tsconfig.json is preserved"
        );

        fs::remove_dir_all(&dir).ok();
    }
}
