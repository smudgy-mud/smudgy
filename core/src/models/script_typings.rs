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
use crate::models::state_exposure::{BoundPath, KnownGlobal, ResolvedExposure, StateExposure};
use crate::session::runtime::{PlatformProducer, ProducerKey};
use anyhow::{Context, Result};
use include_dir::{Dir, DirEntry, File, include_dir};
use std::{borrow::Cow, fs, path::Path, sync::LazyLock};

/// The managed ambient declarations for the `smudgy:core` module. Embedded at build
/// time, rewritten into each server's `.smudgy/types/` on session start.
const SMUDGY_CORE_DTS: &str = include_str!("script_typings/smudgy-core.d.ts");

/// The managed ambient declarations for the per-package `smudgy:params` module
/// (`get(key)` over the package's configured options).
const SMUDGY_PARAMS_DTS: &str = include_str!("script_typings/smudgy-params.d.ts");

/// The managed ambient declarations for the `mapper` API (`Mapper`/`Area`/`Room`/`Exit`/...),
/// declared as global ambient types so scripts can annotate map values without imports and
/// `smudgy:core`'s typed `mapper`/`Area` exports can reference them. Embedded at build time,
/// rewritten on session start.
const SMUDGY_MAPPER_DTS: &str = include_str!("script_typings/smudgy-mapper.d.ts");

/// The managed ambient declarations for `smudgy:widgets` + `smudgy:widgets/jsx-runtime`
/// (the script-driven UI surface + the automatic-JSX runtime and its `JSX` namespace, so
/// `.tsx` authoring type-checks). Embedded at build time, rewritten on session start.
const SMUDGY_WIDGETS_DTS: &str = include_str!("script_typings/smudgy-widgets.d.ts");

/// The managed online Web Audio declarations. This file is written only for a
/// session whose engine received an explicit audio scope; feature compilation
/// alone is not runtime availability.
const SMUDGY_WEB_AUDIO_DTS: &str = include_str!("script_typings/smudgy-web-audio.d.ts");

/// The managed ambient declarations for the global `Worker` surface: main-isolate
/// background workers, always available, so this file is written unconditionally.
/// The vendored Deno lib's own `Worker` declarations are trimmed at vendor time
/// (see [`DENO_LIB`]) so this narrower contract — module workers only, options
/// required — is the single global declaration.
const SMUDGY_WORKERS_DTS: &str = include_str!("script_typings/smudgy-workers.d.ts");

const SMUDGY_INLINE_DTS: &str = include_str!("script_typings/smudgy-inline.d.ts");

/// Returns the inline-automation ambient bridge for an inline-only generated project source.
///
/// The bridge is intentionally absent from [`embedded_language_service_types`], including as
/// a non-root library. Supplying the declaration only as a scoped project source prevents a
/// module or package document from opting into inline-only globals with a path reference.
#[must_use]
pub const fn language_service_inline_bridge() -> &'static str {
    SMUDGY_INLINE_DTS
}

/// The helper type the generated bridge hops an exposed platform path with: the member `K`
/// of `T` once `T` is known to be present, `unknown` where the declared tree has no such
/// member (an undeclared message, or a hop into a scalar), so a path outside the curated
/// shapes reads as `unknown`, as it does through `smudgy:state/gmcp`, instead of failing
/// the declaration.
const STATE_HOP_TYPE: &str = "type SmudgyStateHop<T, K extends string> = \
    K extends keyof NonNullable<T> ? NonNullable<T>[K] : unknown;\n";

/// The comment above the generated bridge's exposure declarations.
const STATE_DECLARATIONS_DOC: &str = "\
/**
 * The state values this automation reads (its \"What it reads\" list), one name each: a
 * platform tree typed along its exposed paths, a package or user handle as `any`. A name
 * that takes a user-API member replaces that member's declaration above, so inside this
 * body the name means the value.
 */
";

/// Returns the inline-automation ambient bridge for one automation: the fixed
/// [`language_service_inline_bridge`] text when `exposures` is empty, else that text minus
/// every global whose name an exposure takes, plus a `declare global` block declaring each
/// exposed name. Removing the shadowed global is what makes the redeclaration legal and the
/// completions truthful: a body exposing `gmcp` sees the state tree, not the protocol
/// control object, exactly as its inner `with` scope does at fire time.
///
/// A platform producer's name is typed along its exposed paths by indexed access from the
/// declared tree (`GmcpTree`, `MsdpTree`, `MsspVariables`): every intermediate is an object,
/// the exposed node is the tree's member there or `undefined`, and exposing the root is the
/// tree itself or `undefined`. A package or `user` handle is `any`. Exposures that do not
/// resolve, or whose name repeats an earlier entry's, are left out, as the runtime leaves
/// them unbound; a reserved-word name never resolves
/// ([`super::state_exposure::JS_RESERVED_NAMES`]), so no declaration is attempted for it.
/// A name the embedded libs declare as a global ([`lib_global_collides`]) is left
/// undeclared too: the redeclaration would error inside the bridge.
#[must_use]
pub fn language_service_inline_bridge_for(exposures: &[StateExposure]) -> Cow<'static, str> {
    use std::fmt::Write as _;

    let mut exposed: Vec<ResolvedExposure> = Vec::with_capacity(exposures.len());
    for exposure in exposures {
        let Ok(resolved) = exposure.resolve() else {
            continue;
        };
        if lib_global_collides(&resolved.name)
            || exposed
                .iter()
                .any(|kept| kept.name.eq_ignore_ascii_case(&resolved.name))
        {
            continue;
        }
        exposed.push(resolved);
    }
    if exposed.is_empty() {
        return Cow::Borrowed(SMUDGY_INLINE_DTS);
    }

    let mut declarations = String::new();
    let mut trees: Vec<&'static str> = Vec::new();
    let mut hops = false;
    for exposure in &exposed {
        let _ = write!(declarations, "  const {}: ", exposure.name);
        match &exposure.producer {
            ProducerKey::Platform(producer) => {
                let tree = platform_tree_type(*producer);
                if !trees.contains(&tree) {
                    trees.push(tree);
                }
                let mut paths = exposure.bound_paths();
                paths.sort_by(|a, b| a.segments.cmp(&b.segments));
                hops |= paths.iter().any(|path| !path.segments.is_empty());
                write_state_shape(&mut declarations, tree, 0, &paths);
            }
            ProducerKey::User | ProducerKey::Package { .. } => declarations.push_str("any"),
        }
        declarations.push_str(";\n");
    }
    trees.sort_unstable();

    let mut bridge = String::with_capacity(SMUDGY_INLINE_DTS.len() + declarations.len() + 1024);
    let mut placed = false;
    let place = |bridge: &mut String| {
        if hops {
            bridge.push_str(STATE_HOP_TYPE);
            bridge.push('\n');
        }
        bridge.push_str(STATE_DECLARATIONS_DOC);
        bridge.push_str("declare global {\n");
        bridge.push_str(&declarations);
        bridge.push_str("}\n\n");
    };
    for line in SMUDGY_INLINE_DTS.lines() {
        if let Some(name) = inline_bridge_global(line)
            && exposed.iter().any(|kept| kept.name == name)
        {
            continue;
        }
        if line == "export {};" && !placed {
            place(&mut bridge);
            placed = true;
        }
        bridge.push_str(line);
        bridge.push('\n');
        if line.starts_with("import type ") && !trees.is_empty() {
            let _ = writeln!(
                bridge,
                "import type {{ {} }} from \"smudgy:core\";",
                trees.join(", ")
            );
        }
    }
    if !placed {
        bridge.push('\n');
        place(&mut bridge);
    }
    Cow::Owned(bridge)
}

/// The name a line of the fixed bridge declares as a global (`const send: …;`), if any.
fn inline_bridge_global(line: &str) -> Option<&str> {
    let declaration = line.trim().strip_prefix("const ")?;
    let (name, _) = declaration.split_once(':')?;
    Some(name.trim())
}

/// The `smudgy:core` interface describing a platform producer's tree.
const fn platform_tree_type(producer: PlatformProducer) -> &'static str {
    match producer {
        PlatformProducer::Gmcp => "GmcpTree",
        PlatformProducer::Msdp => "MsdpTree",
        PlatformProducer::Mssp => "MsspVariables",
    }
}

/// Writes the type of one exposed platform name from `paths`, sorted by segments, all
/// sharing their first `depth` segments, none a prefix of another: the node's own type where
/// the one remaining path ends, else an object literal with one member per next segment.
/// The node's type hops from the tree along the path with [`STATE_HOP_TYPE`] and admits
/// `undefined`, which the exposed node is while the store holds nothing there.
fn write_state_shape(out: &mut String, tree: &str, depth: usize, paths: &[BoundPath]) {
    use std::fmt::Write as _;

    if let [path] = paths
        && path.segments.len() == depth
    {
        out.push_str(&"SmudgyStateHop<".repeat(path.segments.len()));
        out.push_str(tree);
        for segment in &path.segments {
            let _ = write!(out, ", {}>", ts_string(segment));
        }
        out.push_str(" | undefined");
        return;
    }
    out.push_str("{ ");
    let mut start = 0;
    let mut first = true;
    while start < paths.len() {
        let Some(key) = paths[start].segments.get(depth) else {
            start += 1;
            continue;
        };
        let end = start
            + paths[start..]
                .iter()
                .take_while(|path| path.segments.get(depth) == Some(key))
                .count();
        if !first {
            out.push_str("; ");
        }
        first = false;
        if StateExposure::is_identifier(key) {
            out.push_str(key);
        } else {
            out.push_str(&ts_string(key));
        }
        out.push_str(": ");
        write_state_shape(out, tree, depth + 1, &paths[start..end]);
        start = end;
    }
    out.push_str(" }");
}

/// `text` as a double-quoted TypeScript string literal (JSON escaping is a subset of it).
fn ts_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("{text:?}"))
}

/// The vendored Deno runtime lib (`Deno` namespace + web globals like `fetch`/`Response`),
/// with the `/// <reference>` directives stripped so they're plain ambient declarations,
/// and the global `Worker` declarations trimmed — the sibling `smudgy-workers.d.ts`
/// ([`SMUDGY_WORKERS_DTS`]) is the authoritative, narrower contract for that surface.
/// Generic typed-array annotations introduced after TypeScript 5.6 are downleveled to
/// their non-generic equivalents so the embedded 5.6 compiler preserves concrete types.
/// Materialized to `<server>/.smudgy/types/deno/` (covered by the user tsconfig's
/// `.smudgy/types/**` include).
static DENO_LIB: Dir =
    include_dir!("$CARGO_MANIFEST_DIR/src/models/script_typings/vendor/deno-lib");

/// The vendored window lib, the one file the embedded snapshot rewrites.
const WINDOW_LIB_FILE: &str = "lib.deno.window.d.ts";

/// Window-scope declarations left out of the embedded window lib: the blocking stdin
/// dialogs (`alert`, `confirm`, `prompt`) and `location`, which the smudgy runtime does not
/// serve. An ambient bridge declaration cannot shadow a lib global (TypeScript rejects the
/// redeclaration), so leaving these out lets a state handle take the name; `prompt` is the
/// plan's own example. Every other lib global keeps its declaration, and an exposure under
/// such a name is left undeclared by the bridge instead ([`lib_global_collides`]).
const WINDOW_LIB_OMITTED: &[&str] = &["alert", "confirm", "location", "prompt"];

/// The embedded window lib with [`WINDOW_LIB_OMITTED`] removed: each single-line
/// `declare function`/`declare var` for those names, with the doc comment directly above it.
static WINDOW_LIB_TEXT: LazyLock<String> = LazyLock::new(|| {
    let text = DENO_LIB
        .get_file(WINDOW_LIB_FILE)
        .and_then(File::contents_utf8)
        .expect("the vendored Deno lib ships its window declarations");
    strip_global_declarations(text, WINDOW_LIB_OMITTED)
});

/// Removes the single-line `declare function NAME(` / `declare var NAME:` declarations for
/// `names` from a lib text, each with the `/** … */` block directly above it. A multi-line
/// declaration is never touched, so a name whose declaration spans lines stays declared.
fn strip_global_declarations(text: &str, names: &[&str]) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = vec![true; lines.len()];
    for (index, line) in lines.iter().enumerate() {
        let Some(rest) = line
            .strip_prefix("declare function ")
            .or_else(|| line.strip_prefix("declare var "))
        else {
            continue;
        };
        let name_len = rest
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'$')
            .count();
        let (name, after) = rest.split_at(name_len);
        let terminated = matches!(after.as_bytes().first(), Some(b'(' | b':'));
        if !terminated || !names.contains(&name) || !line.trim_end().ends_with(';') {
            continue;
        }
        keep[index] = false;
        // The doc block directly above: ` * …` lines up to and including the `/**` line.
        let mut above = index;
        while above > 0 && lines[above - 1].trim_start().starts_with('*') {
            above -= 1;
        }
        if above > 0 && lines[above - 1].trim_start().starts_with("/**") {
            for slot in &mut keep[above - 1..index] {
                *slot = false;
            }
        }
    }
    let mut out = String::with_capacity(text.len());
    for (line, keep) in lines.iter().zip(keep) {
        if keep {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// A vendored declaration file's text as the language service and the external-editor
/// materialization see it: verbatim, except the window lib, which drops
/// [`WINDOW_LIB_OMITTED`].
fn embedded_type_text(file: &'static File<'static>) -> &'static str {
    if file.path() == Path::new(WINDOW_LIB_FILE) {
        return WINDOW_LIB_TEXT.as_str();
    }
    file.contents_utf8()
        .expect("embedded script declarations must be UTF-8")
}

/// Whether an exposure named `name` would collide with a global the embedded libs still
/// declare (an ECMAScript or Deno global the window lib keeps): the ambient redeclaration
/// would be a TypeScript error inside the bridge, so the bridge leaves the name undeclared
/// and the body sees the lib's type, as the editor's note warns. The names the window lib
/// drops are free to declare.
fn lib_global_collides(name: &str) -> bool {
    matches!(
        StateExposure::known_global(name),
        Some(KnownGlobal::JavaScript | KnownGlobal::Deno)
    ) && !WINDOW_LIB_OMITTED.contains(&name)
}

/// The vendored `@types/node` tree (types `node:events`, `node:path`, … + Node globals),
/// resolved by the base tsconfig's `types`/`typeRoots`. Materialized to
/// `<server>/.smudgy/node-types/@types/node/`.
static NODE_TYPES: Dir =
    include_dir!("$CARGO_MANIFEST_DIR/src/models/script_typings/vendor/node-types");

/// One immutable declaration file exposed to Smudgy's in-process authoring service.
///
/// The virtual path is relative to the language service's private root. `is_root`
/// means TypeScript should include the declaration in every authoring Program; the
/// remaining files are dependencies reached by triple-slash references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedScriptTypeFile {
    pub virtual_path: String,
    pub contents: &'static str,
    pub is_root: bool,
}

/// Returns the static, application-aware declaration snapshot for in-app editing.
///
/// This is deliberately side-effect-free: it reads only declarations embedded in the
/// binary and neither materializes the external-editor project nor consults a package
/// cache. Session-authorized Web Audio and generated installed-package declarations are
/// excluded until the authoring graph has an explicit capability/package snapshot.
#[must_use]
pub fn embedded_language_service_types() -> Vec<EmbeddedScriptTypeFile> {
    let mut files = vec![
        EmbeddedScriptTypeFile {
            virtual_path: "smudgy/smudgy-core.d.ts".to_owned(),
            contents: SMUDGY_CORE_DTS,
            is_root: true,
        },
        EmbeddedScriptTypeFile {
            virtual_path: "smudgy/smudgy-params.d.ts".to_owned(),
            contents: SMUDGY_PARAMS_DTS,
            is_root: true,
        },
        EmbeddedScriptTypeFile {
            virtual_path: "smudgy/smudgy-mapper.d.ts".to_owned(),
            contents: SMUDGY_MAPPER_DTS,
            is_root: true,
        },
        EmbeddedScriptTypeFile {
            virtual_path: "smudgy/smudgy-widgets.d.ts".to_owned(),
            contents: SMUDGY_WIDGETS_DTS,
            is_root: true,
        },
        EmbeddedScriptTypeFile {
            virtual_path: "smudgy/smudgy-workers.d.ts".to_owned(),
            contents: SMUDGY_WORKERS_DTS,
            is_root: true,
        },
    ];
    // TypeScript resolves `/// <reference lib="deno.ns" />` at the canonical
    // `/lib.deno.ns.d.ts` name. Keep those names at the virtual root even though
    // the external-editor materialization remains organized under `types/deno/`.
    append_embedded_type_dir(&mut files, "", &DENO_LIB, &|_| true);
    append_embedded_type_dir_filtered(
        &mut files,
        "node-types",
        &NODE_TYPES,
        &embedded_node_type_is_included,
        &|path| path == Path::new("@types/node/ts5.6/index.d.ts"),
    );
    files.sort_by(|left, right| left.virtual_path.cmp(&right.virtual_path));
    files
}

fn embedded_node_type_is_included(path: &Path) -> bool {
    let path = path.to_string_lossy().replace('\\', "/");

    path != "@types/node/index.d.ts"
        && path != "@types/node/globals.typedarray.d.ts"
        && path != "@types/node/buffer.buffer.d.ts"
        && !path.starts_with("@types/node/ts5.7/")
        && (!path.starts_with("@types/node/web-globals/")
            || path == "@types/node/web-globals/timers.d.ts")
}

fn append_embedded_type_dir(
    output: &mut Vec<EmbeddedScriptTypeFile>,
    prefix: &str,
    directory: &'static Dir<'static>,
    is_root: &impl Fn(&Path) -> bool,
) {
    append_embedded_type_dir_filtered(output, prefix, directory, &|_| true, is_root);
}

fn append_embedded_type_dir_filtered(
    output: &mut Vec<EmbeddedScriptTypeFile>,
    prefix: &str,
    directory: &'static Dir<'static>,
    include: &impl Fn(&Path) -> bool,
    is_root: &impl Fn(&Path) -> bool,
) {
    for file in directory.files() {
        let path = file.path();
        if !include(path)
            || !path
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("ts"))
            || !path
                .file_stem()
                .map(Path::new)
                .and_then(Path::extension)
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("d"))
        {
            continue;
        }
        let relative = path.to_string_lossy().replace('\\', "/");
        output.push(EmbeddedScriptTypeFile {
            virtual_path: if prefix.is_empty() {
                relative
            } else {
                format!("{prefix}/{relative}")
            },
            contents: embedded_type_text(file),
            is_root: is_root(path),
        });
    }
    for child in directory.dirs() {
        append_embedded_type_dir_filtered(output, prefix, child, include, is_root);
    }
}

/// Bumped whenever the vendored Deno lib / `@types/node` change, so the runtime typings are
/// (re)written only on first run or after a re-vendor — not on every session start.
const RUNTIME_TYPES_VERSION: &str = "deno-v2.9.5+node-26.0.1+3";

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
                let _ = writeln!(
                    out,
                    "  export {{ __handle as {} }};",
                    export_name(&handle.name)
                );
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
            InteropKind::Procedure => {
                "import(\"smudgy:core\").ProcedureConsumer<unknown>".to_string()
            }
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
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "new",
        "null",
        "return",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "implements",
        "interface",
        "let",
        "package",
        "private",
        "protected",
        "public",
        "static",
        "yield",
        "await",
        // Built-in type names tsc refuses as alias names.
        "any",
        "unknown",
        "never",
        "object",
        "string",
        "number",
        "boolean",
        "symbol",
        "bigint",
        "undefined",
        "intrinsic",
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
    ensure_script_tsconfig_with_packages_and_web_audio(server_name, packages, false)
}

/// Refreshes the managed project using actual engine authority rather than the
/// Cargo feature as the Web Audio availability signal.
pub(crate) fn ensure_script_tsconfig_with_packages_and_web_audio(
    server_name: &str,
    packages: &[InstalledPackageTypes],
    web_audio_available: bool,
) -> Result<()> {
    let server_dir = get_smudgy_home()?.join(server_name);
    ensure_script_tsconfig_in_with_web_audio(&server_dir, packages, web_audio_available)
}

/// [`ensure_script_tsconfig_with_packages`] against an explicit server directory (test seam).
#[cfg(test)]
fn ensure_script_tsconfig_in(server_dir: &Path, packages: &[InstalledPackageTypes]) -> Result<()> {
    ensure_script_tsconfig_in_with_web_audio(server_dir, packages, false)
}

fn ensure_script_tsconfig_in_with_web_audio(
    server_dir: &Path,
    packages: &[InstalledPackageTypes],
    web_audio_available: bool,
) -> Result<()> {
    let managed_dir = server_dir.join(".smudgy");
    let types_dir = managed_dir.join("types");
    fs::create_dir_all(&types_dir).with_context(|| format!("create {}", types_dir.display()))?;

    write_if_changed(&managed_dir.join("README.md"), MANAGED_README)?;
    write_if_changed(
        &managed_dir.join("tsconfig.base.json"),
        &tsconfig_base(packages)?,
    )?;
    write_if_changed(&types_dir.join("smudgy-core.d.ts"), SMUDGY_CORE_DTS)?;
    write_if_changed(&types_dir.join("smudgy-params.d.ts"), SMUDGY_PARAMS_DTS)?;
    write_if_changed(&types_dir.join("smudgy-mapper.d.ts"), SMUDGY_MAPPER_DTS)?;
    write_if_changed(&types_dir.join("smudgy-widgets.d.ts"), SMUDGY_WIDGETS_DTS)?;
    write_if_changed(&types_dir.join("smudgy-workers.d.ts"), SMUDGY_WORKERS_DTS)?;
    let web_audio_types = types_dir.join("smudgy-web-audio.d.ts");
    if web_audio_available {
        write_if_changed(&web_audio_types, SMUDGY_WEB_AUDIO_DTS)?;
    } else if let Err(error) = fs::remove_file(&web_audio_types)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(error).with_context(|| format!("remove {}", web_audio_types.display()));
    }
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
    fs::create_dir_all(&modules_dir)
        .with_context(|| format!("create {}", modules_dir.display()))?;
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
                // The window lib is written as the in-app service sees it, so the two
                // editors agree on which globals exist.
                let contents: &[u8] = if file.path() == Path::new(WINDOW_LIB_FILE) {
                    WINDOW_LIB_TEXT.as_bytes()
                } else {
                    file.contents()
                };
                if fs::read(&path).is_ok_and(|existing| existing == contents) {
                    continue;
                }
                fs::write(&path, contents).with_context(|| format!("write {}", path.display()))?;
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
        assert!(
            dir.join(".smudgy/types/smudgy-workers.d.ts").is_file(),
            "the Worker surface is unconditional: main-isolate workers always exist"
        );
        assert!(
            !dir.join(".smudgy/types/smudgy-web-audio.d.ts").exists(),
            "an audio-free engine must not advertise Web Audio"
        );
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
        assert!(core.contains("export const Area:"));

        // The mapper + widgets ambient declarations ship alongside.
        let mapper = fs::read_to_string(dir.join(".smudgy/types/smudgy-mapper.d.ts")).unwrap();
        assert!(mapper.contains("interface Mapper"));
        assert!(mapper.contains("interface Area"));
        assert!(!mapper.contains("declare const mapper: Mapper"));
        assert!(!mapper.contains("declare class Area"));
        let widgets = fs::read_to_string(dir.join(".smudgy/types/smudgy-widgets.d.ts")).unwrap();
        assert!(widgets.contains("declare module \"smudgy:widgets\""));
        assert!(widgets.contains("declare module \"smudgy:widgets/jsx-runtime\""));
        assert!(widgets.contains("namespace JSX"));
        let workers = fs::read_to_string(dir.join(".smudgy/types/smudgy-workers.d.ts")).unwrap();
        assert!(workers.contains("interface Worker extends EventTarget"));
        assert!(workers.contains("declare var Worker"));

        // The base tsconfig wires the automatic JSX runtime for `.tsx` widget authoring.
        let base = fs::read_to_string(dir.join(".smudgy/tsconfig.base.json")).unwrap();
        assert!(base.contains("react-jsx"), "jsx setting missing:\n{base}");
        assert!(
            base.contains("smudgy:widgets"),
            "jsxImportSource missing:\n{base}"
        );

        // The installed-events barrel is always written (header-only when no packages).
        assert!(dir.join(".smudgy/types/installed-events.d.ts").is_file());

        // The seeded tsconfig covers both subtrees.
        let user = fs::read_to_string(dir.join("tsconfig.json")).unwrap();
        assert!(user.contains("./.smudgy/tsconfig.base.json"));
        assert!(user.contains("modules/**/*"));
        assert!(user.contains("packages/**/*"));

        // The `modules/` subtree is seeded as its own thin project pointing one level up.
        let modules_ts = fs::read_to_string(dir.join("modules/tsconfig.json")).unwrap();
        assert!(
            modules_ts.contains("../tsconfig.json"),
            "modules tsconfig extends the server project:\n{modules_ts}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn web_audio_typings_follow_session_authority_and_remove_stale_surface() {
        let dir = temp_server_dir("web-audio-authority");
        let declaration = dir.join(".smudgy/types/smudgy-web-audio.d.ts");

        ensure_script_tsconfig_in_with_web_audio(&dir, &[], true)
            .expect("audio-scoped engine writes Web Audio declarations");
        assert_eq!(
            fs::read_to_string(&declaration).unwrap(),
            SMUDGY_WEB_AUDIO_DTS
        );

        ensure_script_tsconfig_in_with_web_audio(&dir, &[], false)
            .expect("audio-free engine removes stale Web Audio declarations");
        assert!(!declaration.exists());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn public_web_audio_examples_compile_against_the_narrow_hosted_surface() {
        use std::collections::BTreeMap;

        let manifest_text =
            include_str!("../../../examples/web_audio_a11y_package/smudgy.package.json");
        smudgy_script::PackageManifest::parse(manifest_text)
            .expect("the complete public package manifest must remain valid for Smudgy");
        let package_manifest: serde_json::Value = serde_json::from_str(manifest_text)
            .expect("the complete public package manifest must remain valid JSON");
        assert_eq!(package_manifest["entry"], "index.ts");
        assert!(package_manifest["permissions"].get("audio").is_none());

        let mut ambient = BTreeMap::new();
        // The declaration emitter normally uses lib.esnext.full (including DOM).
        // Suppress that default here and add only ESNext plus the Deno event seam,
        // so a browser's much wider Web Audio declarations cannot mask drift.
        ambient.insert(
            "000-runtime.d.ts".to_string(),
            "/// <reference no-default-lib=\"true\" />\n\
             /// <reference lib=\"esnext\" />\n\
             interface Event {}\n\
             interface EventTarget {}\n"
                .to_string(),
        );
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert(
            "smudgy-web-audio.d.ts".to_string(),
            SMUDGY_WEB_AUDIO_DTS.to_string(),
        );

        let mut examples = BTreeMap::new();
        examples.insert(
            "web_audio_earcon.ts".to_string(),
            include_str!("../../../examples/web_audio_earcon.ts").to_string(),
        );
        examples.insert(
            "web_audio_a11y_package.ts".to_string(),
            include_str!("../../../examples/web_audio_a11y_package/index.ts").to_string(),
        );
        let output = smudgy_script::dts::generate_declarations(&examples, &ambient)
            .expect("compile public examples against hosted declarations");
        assert!(
            output.diagnostics.is_empty(),
            "public Web Audio examples drifted from hosted declarations:\n{:#?}",
            output.diagnostics
        );

        let mut unsupported = BTreeMap::new();
        unsupported.insert(
            "unsupported.ts".to_string(),
            "const context = new AudioContext({ sinkId: \"none\" });\n\
             context.createBuffer(1, 128, 48_000);\n\
             context.createGain().gain.setValueAtTime(0.5, 0);\n\
             new OfflineAudioContext(1, 128, 48_000);\n\
             new BiquadFilterNode(context);\n\
             context.createOscillator().type = \"custom\";\n"
                .to_string(),
        );
        let output = smudgy_script::dts::generate_declarations(&unsupported, &ambient)
            .expect("unsupported surface produces ordinary diagnostics");
        assert_eq!(
            output.diagnostics.len(),
            5,
            "only the five deliberate unsupported uses should diagnose: {:#?}",
            output.diagnostics
        );
        for expected in [
            "Property 'createBuffer' does not exist on type 'AudioContext'.",
            "Property 'setValueAtTime' does not exist on type 'AudioParam'.",
            "Cannot find name 'OfflineAudioContext'.",
            "Cannot find name 'BiquadFilterNode'.",
            "Type '\"custom\"' is not assignable to type 'OscillatorType'.",
        ] {
            assert!(
                output.diagnostics.iter().any(|actual| actual == expected),
                "missing exact diagnostic {expected:?}: {:#?}",
                output.diagnostics
            );
        }
    }

    /// The shipped worker declarations: a consumer exercising the whole
    /// parent-side bridge (construction, both `postMessage` shapes, the handler
    /// properties, the listener overloads, `terminate`) compiles cleanly, while
    /// the unsupported spellings (an options-less construction, a "classic"
    /// worker) stay compile errors. The vendored Deno lib's own `Worker` global
    /// is trimmed so this contract is the single declaration; the last assertion
    /// keeps a re-vendor from silently reintroducing the wide one.
    #[test]
    fn worker_typings_compile_and_stay_narrow() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        // The declaration emitter normally uses lib.esnext.full (including DOM,
        // whose own `Worker` would mask drift). Suppress that default and add
        // only ESNext plus stubs for the event/URL seams the contract references
        // — in the editor project these come from the vendored runtime typings.
        ambient.insert(
            "000-runtime.d.ts".to_string(),
            "/// <reference no-default-lib=\"true\" />\n\
             /// <reference lib=\"esnext\" />\n\
             interface Event {}\n\
             interface EventListener { (event: Event): void; }\n\
             interface EventListenerObject { handleEvent(event: Event): void; }\n\
             type EventListenerOrEventListenerObject = EventListener | EventListenerObject;\n\
             interface EventTarget {\n\
               addEventListener(type: string, listener: EventListenerOrEventListenerObject | null, options?: boolean | AddEventListenerOptions): void;\n\
               dispatchEvent(event: Event): boolean;\n\
               removeEventListener(type: string, listener: EventListenerOrEventListenerObject | null, options?: boolean | EventListenerOptions): void;\n\
             }\n\
             interface MessageEvent<T = any> extends Event { readonly data: T; }\n\
             interface ErrorEvent extends Event { readonly message: string; }\n\
             interface URL {}\n\
             type Transferable = ArrayBuffer;\n\
             interface AddEventListenerOptions { once?: boolean; }\n\
             interface EventListenerOptions { capture?: boolean; }\n"
                .to_string(),
        );
        ambient.insert(
            "smudgy-workers.d.ts".to_string(),
            SMUDGY_WORKERS_DTS.to_string(),
        );

        let mut sources = BTreeMap::new();
        sources.insert(
            "worker_consumer.ts".to_string(),
            "const tally = new Worker(\n\
               \"data:text/javascript,self.onmessage = () => {};\",\n\
               { type: \"module\", name: \"tally\" },\n\
             );\n\
             tally.postMessage([\"a slime is DEAD!\"]);\n\
             const buffer = new ArrayBuffer(8);\n\
             tally.postMessage(buffer, [buffer]);\n\
             tally.onmessage = (e) => { void e.data; };\n\
             tally.onmessageerror = (e) => { void e.data; };\n\
             tally.onerror = (e) => { const m: string = e.message; void m; };\n\
             tally.addEventListener(\"message\", (e) => { void e.data; }, { once: true });\n\
             tally.addEventListener(\"messageerror\", (e) => { void e.data; });\n\
             tally.addEventListener(\"error\", (e) => { void e.message; });\n\
             tally.removeEventListener(\"message\", (e) => { void e.data; });\n\
             const eventTarget: EventTarget = tally;\n\
             eventTarget.dispatchEvent({} as Event);\n\
             tally.terminate();\n\
             export {};\n"
                .to_string(),
        );
        let output = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("compile the worker consumer against the hosted declarations");
        assert!(
            output.diagnostics.is_empty(),
            "the shipped smudgy-workers.d.ts produced diagnostics:\n{:#?}",
            output.diagnostics
        );

        let mut unsupported = BTreeMap::new();
        unsupported.insert(
            "unsupported.ts".to_string(),
            "new Worker(\"data:text/javascript,;\");\n\
             new Worker(\"data:text/javascript,;\", { type: \"classic\" });\n\
             export {};\n"
                .to_string(),
        );
        let output = smudgy_script::dts::generate_declarations(&unsupported, &ambient)
            .expect("unsupported surface produces ordinary diagnostics");
        assert_eq!(
            output.diagnostics.len(),
            2,
            "only the two deliberate unsupported constructions should diagnose: {:#?}",
            output.diagnostics
        );

        let deno_shared = DENO_LIB
            .get_file("lib.deno.shared_globals.d.ts")
            .expect("the vendored Deno lib ships its shared globals")
            .contents_utf8()
            .expect("the vendored Deno lib is UTF-8");
        assert!(
            !deno_shared.contains("declare var Worker")
                && !deno_shared.contains("interface Worker extends EventTarget"),
            "the vendored Deno lib declares a global `Worker` again — trim it so \
             smudgy-workers.d.ts stays the single (narrower) declaration"
        );
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
        assert!(
            !bare.contains("\"paths\""),
            "unexpected paths block:\n{bare}"
        );

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
            barrel.contains(
                "/// <reference path=\"../packages/kapusniak/arctic-prompt/index.ts\" />"
            ),
            "package reference missing:\n{barrel}"
        );

        // Re-running with an empty set fully regenerates the barrel — the stale reference
        // self-prunes, leaving a header-only file.
        ensure_script_tsconfig_in(&dir, &[]).expect("ensure empty");
        let bare = fs::read_to_string(dir.join(".smudgy/types/installed-events.d.ts")).unwrap();
        assert!(
            !bare.contains("arctic-prompt"),
            "stale reference lingered:\n{bare}"
        );

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
        assert!(
            shims.contains("declare module \"smudgy:state/kapusniak/arctic-prompt/promptState\"")
        );
        assert!(shims.contains("declare module \"smudgy:events/kapusniak/arctic-prompt\""));
        assert!(shims.contains("declare module \"smudgy:procedures/kapusniak/arctic-prompt\""));
        assert!(
            shims.contains(
                "ConsumerOf<typeof import(\"smudgy://kapusniak/arctic-prompt\").promptState>"
            ),
            "an exported handle derives from typeof its declaration:\n{shims}"
        );
        assert!(
            shims.contains(
                "ConsumerOf<import(\"smudgy://kapusniak/arctic-prompt\").RefreshRequest>"
            ),
            "a module-local handle still derives via its erased alias:\n{shims}"
        );
        assert!(
            shims.contains("EventConsumer<unknown>"),
            "alias-less handle must fall back to an unknown payload:\n{shims}"
        );
        assert!(
            shims.contains(
                "export type promptState = import(\"smudgy:core\").Payload<typeof __h0>;"
            ),
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
            shims.contains(
                "export type Payload = import(\"smudgy:core\").Payload<typeof __handle>;"
            ),
            "subpath modules export the fixed-name Payload:\n{shims}"
        );

        // The shim + a consumer must compile against the real contract. The producer module
        // stands in for the materialized entry source `smudgy://…` resolves to.
        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );
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

    /// The declaration-grammar conformance fixture (interop.md §4/§5): FOUR
    /// independent mechanisms parse producer declarations — transpile-time name
    /// injection, static extraction, the typings generator, and the non-home scrub. One
    /// golden source, asserted against all four, so they can never drift on what a
    /// declaration means.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn declaration_grammar_conformance_across_all_consumers() {
        use smudgy_script::interop_extract::{
            InteropKind, extract_interop_handles, inject_inferred_handle_names,
            scrub_handle_exports,
        };

        const FIXTURE: &str = r#"
import { createState, createEvent, createProcedure, createDerived } from "smudgy:core";
export interface VitalData { hp: number }

/** The current vitals reading. */
export const vitals = createState<VitalData>();
export const prompt = createEvent<{ raw: string }>();
export const refresh = createProcedure((args: { full: boolean }, caller) => { void args; void caller.origin; void caller.session; });
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
        assert!(
            extraction.export_diagnostics.is_empty(),
            "{:#?}",
            extraction.export_diagnostics
        );
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
        assert!(
            injected.contains(r#"createState<VitalData>("vitals")"#),
            "{injected}"
        );
        assert!(
            injected.contains(r#"createEvent<{ raw: string }>("prompt")"#),
            "{injected}"
        );
        assert!(
            injected.contains(r#"createProcedure("refresh", (args"#),
            "{injected}"
        );
        assert!(
            injected.contains(r#"createDerived("hpPct", vitals as any"#),
            "{injected}"
        );
        assert!(
            injected.contains(r#"createState("options", { persist: true } as any)"#),
            "{injected}"
        );
        assert!(
            injected.contains("createState<VitalData>('Pinned')"),
            "{injected}"
        );
        assert!(injected.contains("createEvent('dynamic')"), "{injected}");
        assert_eq!(FIXTURE.lines().count(), injected.lines().count());
        let re_extracted = extract_interop_handles(&url, &injected).expect("injected parses");
        let re_summary: Vec<(&str, InteropKind, bool)> = re_extracted
            .handles
            .iter()
            .map(|h| (h.name.as_str(), h.kind, h.exported))
            .collect();
        assert_eq!(
            summary, re_summary,
            "injection must not change what extraction sees"
        );

        // 3. Scrub: every exported handle's export-ness removed (the aliased `pinned` named
        // export too), nothing else touched, line count preserved.
        let (scrubbed, removed) = scrub_handle_exports(&url, FIXTURE).expect("scrubs");
        assert_eq!(
            removed,
            vec!["vitals", "prompt", "refresh", "hpPct", "pinned", "options"]
        );
        assert!(
            scrubbed.contains("export interface VitalData"),
            "{scrubbed}"
        );
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
                shims.contains(&format!(
                    "declare module \"smudgy:{scheme}/wbk/fixture/{name}\""
                )),
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
        assert!(
            shims.contains("/** The current vitals reading. */"),
            "{shims}"
        );
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
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );

        let mut sources = BTreeMap::new();
        sources.insert(
            "producer.ts".to_string(),
            "import { createState, createEvent, createProcedure } from \"smudgy:core\";\n\
             import type { Binding } from \"smudgy:core\";\n\
             export interface PromptData { hp: number; maxhp: number }\n\
             const promptState = createState<PromptData>('promptState');\n\
             const prompt = createEvent<PromptData>('prompt');\n\
             const refreshRequest = createProcedure('refreshRequest', (payload: { full: boolean }, caller) => { const f: boolean = payload.full; void f; const who: string = caller.origin; const sourceId: number = caller.session.id; void who; void sourceId; });\n\
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
            "import { events, session, mapper, getSessions, createHotkey, line, createDerived, style } from \"smudgy:core\";\n\
             import type { ConsumerOf, Payload, TextAttributes } from \"smudgy:core\";\n\
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
               for (const s of getSessions()) {\n\
                 s.send(\"x\");\n\
                 const connected: boolean = s.connected; void connected;\n\
                 prompt.from(s).on((p, source) => { void p.hp; const id: number = source.id; void id; });\n\
                 prompt.fromAll({ includeSelf: false }).once((p, source) => { void p.hp; void source.profile; });\n\
                 promptState.from(s).watch((next) => { void next?.hp; });\n\
                 refreshRequest.to(s).post({ full: false });\n\
               }\n\
               createHotkey({ key: \"F1\", modifiers: [\"ctrl\"] }, () => {}).delete();\n\
               createHotkey({ key: \"t\", modifiers: [\"ctrl\"] }, () => {}).delete();\n\
               createHotkey({ key: \"Code(KeyT)\", modifiers: [\"alt\", \"shift\"] }, () => {}).delete();\n\
               createHotkey({ key: \"Character(é)\", modifiers: [\"super\"] }, () => {}).delete();\n\
               const t: string = line.text; void t;\n\
               const attributes: TextAttributes = { bold: true, faint: false, italic: true, underline: \"double\", blink: \"fast\", crossedOut: true, reverse: false };\n\
               void style({ fg: { color: \"red\", bold: true, paletteBright: false }, attributes })`styled`;\n\
               const span = line.styles?.[0];\n\
               if (span) { const raw: boolean | undefined = typeof span.fg === \"object\" && \"paletteBright\" in span.fg ? span.fg.paletteBright : span.foregroundPaletteBright; void raw; line.highlightAt(span.begin, span.end, span); }\n\
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
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );

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

        // `previousValue` is read-only on BOTH seats (interop.md §2): even on
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
        for producer in ["gmcp", "msdp", "mssp"] {
            assert!(
                smudgy_script::platform_state_producer(producer),
                "{producer} is a platform state producer"
            );
            assert!(
                SMUDGY_CORE_DTS.contains(&format!("declare module \"smudgy:state/{producer}\"")),
                "smudgy:state/{producer} missing from smudgy-core.d.ts"
            );
        }
        for producer in ["sys", "map", "gmcp", "msdp", "mssp", "input", "sessions"] {
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
            for (export, _event) in catalog {
                assert!(
                    block.contains(&format!("export const {export}:")),
                    "smudgy:events/{producer} declaration is missing `{export}` (runtime synthesizes it)"
                );
            }
        }
    }

    /// The typed pane creation specs: a split's initial size is keyed to the split
    /// axis, so `width` on a `left`/`right` split (and `height` on `top`/`bottom`) compiles,
    /// while the off-axis dimension is a compile error (`never` on the wrong key). Also
    /// exercises `titleBar` round-tripping through the contract.
    #[test]
    fn pane_spec_keys_size_to_the_split_axis() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );

        let good = "import { session } from \"smudgy:core\";\n\
             import type { TitleBarSpec, InputHandle, GroupWithOptions } from \"smudgy:core\";\n\
             export function wire() {\n\
               const pinned: TitleBarSpec = \"always-show\";\n\
               const chat = session.mainPane.split(\"right\", { name: \"chat\", width: 300, titleBar: pinned,\n\
                 input: { onSubmit: (text: string) => session.send(`gt ${text}`), placeholder: \"group tell...\" } });\n\
               const chatInput: InputHandle | undefined = chat.input;\n\
               chatInput?.propose(\"hello\");\n\
               chat.split(\"bottom\", { name: \"log\", height: 120, terminal: false, titleBar: \"normal\" });\n\
               session.mainPane.split(\"top\", { name: \"status\", height: 80 });\n\
               session.mainPane.split(\"left\", { name: \"map\" });\n\
               const tab = chat.addTab({ name: \"alerts\", selected: true });\n\
               const grouping: GroupWithOptions = { position: \"before\", selected: false };\n\
               tab.groupWith(session.mainPane, grouping);\n\
               tab.select();\n\
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
               session.mainPane.addTab({ name: \"tab\", width: 120 });\n\
               session.mainPane.groupWith(session.mainPane, { position: \"middle\" });\n\
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

    /// Pane scroll deltas accept exactly one unit.
    #[test]
    fn pane_scroll_delta_accepts_exactly_one_unit() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );

        let good = "import { session } from \"smudgy:core\";\n\
             import type { PaneScrollDelta } from \"smudgy:core\";\n\
             const pages: PaneScrollDelta = { pages: -1 };\n\
             const lines: PaneScrollDelta = { lines: 3 };\n\
             session.mainPane.scrollTo(42);\n\
             session.mainPane.scrollTo(\"end\");\n\
             session.mainPane.scrollBy(pages);\n\
             session.mainPane.scrollBy(lines);\n";
        let mut sources = BTreeMap::new();
        sources.insert("consumer.ts".to_string(), good.to_string());
        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("generate the pane-scroll consumer");
        assert!(
            out.diagnostics.is_empty(),
            "valid pane-scroll calls must compile: {:?}",
            out.diagnostics
        );

        for bad in [
            "session.mainPane.scrollBy({ pages: 1, lines: 1 });",
            "session.mainPane.scrollBy({});",
        ] {
            let source = format!("import {{ session }} from \"smudgy:core\";\n{bad}\n");
            let mut sources = BTreeMap::new();
            sources.insert("consumer.ts".to_string(), source);
            let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
                .expect("the generator must handle an invalid pane-scroll delta");
            assert!(
                !out.diagnostics.is_empty(),
                "an invalid pane-scroll delta compiled: {bad}"
            );
        }
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
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );
        ambient.insert(
            "smudgy-widgets.d.ts".to_string(),
            SMUDGY_WIDGETS_DTS.to_string(),
        );

        let mut sources = BTreeMap::new();
        sources.insert(
            "ui.tsx".to_string(),
            "import { createWidget, Column, Row, Text, ProgressBar, Button, MapView } from \"smudgy:widgets\";\n\
             import type { MapStyleApplication, MapDoorState } from \"smudgy:widgets\";\n\
             import { session, createState } from \"smudgy:core\";\n\
             interface Vitals { hp: number; maxhp: number; name: string }\n\
             interface Gps { apply: MapStyleApplication[]; doors: MapDoorState[] }\n\
             const vitals = createState<Vitals>('vitals');\n\
             const untyped = createState('untyped');\n\
             const gps = createState<Gps>('gps');\n\
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
                   <MapView\n\
                     roomSpacing={1.25} playerColor=\"#fff\" showDoors={true}\n\
                     defaultStyle={{ connectionColor: \"#888\", crossAreaLabelVisibility: \"hover\",\n\
                                     crossAreaLabelBackground: \"rgba(0,0,0,0.8)\" }}\n\
                     styles={{ route: { connectionColor: \"gold\", connectionWidth: 2, roomStroke: \"gold\", roomStrokeWidth: 2,\n\
                                        crossAreaLabelVisibility: \"always\" },\n\
                               visited: { roomFill: \"#223\", roomBorderRadius: 0.2, doorColor: \"#f00\" } }}\n\
                     apply={[{ style: \"route\", rooms: [1, 2], exits: [{ room: 1, direction: \"North\" }] },\n\
                             { style: \"visited\", rooms: [9], area: [1, 2] },\n\
                             { style: \"visited\", rooms: [10], area: \"67e55044-10b1-426f-9247-bb680e5fe0c8\" }]}\n\
                     doors={[{ exit: { room: 1, direction: \"North\" }, closed: true, locked: false }]}\n\
                   />\n\
                   <MapView apply={gps.bind('apply')} doors={gps.bind('doors')} roomSpacing={vitals.bind('hp')} />\n\
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
        // A third consumer proves the Canvas contract (shape-record discriminated unions,
        // animate specs, view_box/fit, pointer events, a bound scene) and the form widgets
        // (bound checked, the boolean onToggle, a radio group with a numeric-bindable
        // `selected`) compile as authored.
        sources.insert(
            "hud.tsx".to_string(),
            "import { createWidget, Canvas, Space, Image, Checkbox, Radio, Row, Column, Tooltip, Table, ProgressBar, Text } from \"smudgy:widgets\";\n\
             import type { CanvasShape, CanvasPointerEvent, CanvasFill } from \"smudgy:widgets\";\n\
             import { createState } from \"smudgy:core\";\n\
             interface Cfg { autoloot: boolean; mode: string; slot: number; scene: CanvasShape[] }\n\
             const cfg = createState<Cfg>('cfg');\n\
             export function mountHud() {\n\
               const gradient: CanvasFill = { gradient: { from: [0, 0], to: [100, 0], stops: [[0, \"#000\"], [1, \"#fff\"]] } };\n\
               const scene: CanvasShape[] = [\n\
                 { kind: \"rect\", x: 0, y: 0, width: 100, height: 8, rx: 2, fill: gradient },\n\
                 { kind: \"path\", d: \"M 0 0 A 5 5 0 0 1 10 0 Z\", stroke: { color: \"#fff\", width: 1, dash: [2, 2] } },\n\
                 { kind: \"text\", x: 4, y: 4, text: \"hp\", size: 10, color: \"#8fa\", align_x: \"center\", font: \"monospace\" },\n\
                 { kind: \"image\", src: \"@/assets/map-bg.png\", x: 0, y: 0, width: 100, height: 50,\n\
                   fit: \"cover\", filter: \"nearest\", rotate: 15, opacity: 0.8,\n\
                   animate: { x: { to: 10, duration: 250 }, rotate: { to: 0, duration: 250 } } },\n\
                 { kind: \"group\", transform: { translate: [5, 5], rotate: 45, scale: [1, 2] }, children: [\n\
                   { kind: \"circle\", id: \"ring\", cx: 0, cy: 0, r: 2, transient: true,\n\
                     animate: { r: { to: 100, duration: 500, ease: \"out\", repeat: 2 } } },\n\
                 ] },\n\
               ];\n\
               createWidget(\"hud\",\n\
                 <Column>\n\
                   <Canvas width=\"fill\" height={120} view_box={[0, 0, 100, 50]} fit=\"contain\"\n\
                           scene={cfg.bind('scene')}\n\
                           onPointer={(ev: CanvasPointerEvent) => { void ev.kind; void ev.x; void ev.button; }} />\n\
                   <Canvas scene={scene} />\n\
                   <Image src=\"@/assets/logo.png\" width={64} height={64} content_fit=\"cover\" />\n\
                   <Image src=\"https://example.com/x.png\" opacity={0.5} rotation={45} filter_method=\"nearest\" />\n\
                   <Image src={cfg.bind('mode')} width=\"fill\" height={32} opacity={cfg.bind('slot')} />\n\
                   <Row>\n\
                     <Checkbox checked={cfg.bind('autoloot')} size={14} text_size={12}\n\
                               onToggle={(v) => { cfg.value.autoloot = v; }}>Autoloot: {cfg.bind('autoloot')}</Checkbox>\n\
                     <Tooltip tip=\"mirrors the box beside it\" position=\"right\" gap={4}>\n\
                       <Checkbox checked={true}>read-only</Checkbox>\n\
                     </Tooltip>\n\
                     <Tooltip tip={cfg.bind('mode')} position=\"cursor\">\n\
                       <Text size={11}>mode</Text>\n\
                     </Tooltip>\n\
                     <Tooltip tip={<Column><Text>styled</Text><Text>element tip</Text></Column>}>\n\
                       <Text size={11}>details</Text>\n\
                     </Tooltip>\n\
                     <Tooltip tip={false}>\n\
                       <Text size={11}>conditional: no tooltip</Text>\n\
                     </Tooltip>\n\
                     <Space width=\"fill\" />\n\
                     <Radio value=\"fast\" selected={cfg.bind('mode')} onSelect={(v) => { cfg.value.mode = v; }}>Fast</Radio>\n\
                     <Radio value={2} selected={cfg.bind('slot')} size={12} onSelect={(v) => { void v; }}>Slot two</Radio>\n\
                   </Row>\n\
                   <Table\n\
                     columns={[\n\
                       { header: \"Name\", width: 120 },\n\
                       { header: <Text size={10}>HP</Text>, width: \"fill\", align_x: \"center\" },\n\
                       { header: \"MV\", align_y: \"center\" },\n\
                     ]}\n\
                     rows={[\n\
                       [\"Mora\", <ProgressBar value={cfg.bind('slot')} max={100} />, cfg.bind('mode')],\n\
                       [\"Kessik\", null, 42],\n\
                     ]}\n\
                     width=\"fill\" padding={4} separator={1}\n\
                   />\n\
                 </Column>,\n\
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
    /// `Binding<number>` and the untyped `Binding<any>`, not a known-wrong payload. Also
    /// covers required props: an `<Image />` without `src` must fail to compile.
    #[test]
    fn mistyped_binding_props_fail_to_compile() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );
        ambient.insert(
            "smudgy-widgets.d.ts".to_string(),
            SMUDGY_WIDGETS_DTS.to_string(),
        );

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

        // A required prop left off: `<Image />` without `src` must fail to compile.
        let mut sources = BTreeMap::new();
        sources.insert(
            "img.tsx".to_string(),
            "import { Image } from \"smudgy:widgets\";\n\
             export const bad = <Image />;\n"
                .to_string(),
        );
        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("the generator itself must not crash on a type error");
        assert!(
            !out.diagnostics.is_empty(),
            "an <Image /> without the required `src` must be a compile error"
        );
    }

    #[test]
    fn invalid_hotkey_keys_and_modifiers_fail_to_compile() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );

        for (name, source) in [
            (
                "bad-key.ts",
                "import { createHotkey } from \"smudgy:core\";\n\
                 createHotkey({ key: \"DefinitelyNotAKey\" }, () => {});\n",
            ),
            (
                "bad-modifier.ts",
                "import { createHotkey } from \"smudgy:core\";\n\
                 createHotkey({ key: \"t\", modifiers: [\"command\"] }, () => {});\n",
            ),
        ] {
            let mut sources = BTreeMap::new();
            sources.insert(name.to_string(), source.to_string());
            let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
                .expect("the generator itself must not crash on a type error");
            assert!(
                !out.diagnostics.is_empty(),
                "{name} must fail the strongly typed createHotkey contract"
            );
        }
    }

    /// The trigger-specific style surface deliberately reuses the callable style builder
    /// without widening the shared `Pattern` type used by aliases. Compile every supported
    /// builder/decorator form as a consumer so an overload cannot silently drift away from the
    /// runtime implementation.
    #[test]
    fn styled_trigger_surface_compiles() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );

        let mut sources = BTreeMap::new();
        sources.insert(
            "styled-triggers.ts".to_string(),
            r#"import {
  createTrigger,
  createTriggers,
  pattern,
  style,
  type RgbColor,
  type StyleMatch,
  type StyleMatchBuilder,
  type TriggerPattern,
  type TriggerPatterns,
} from "smudgy:core";

const crimson: RgbColor = { r: 220, g: 20, b: 60 };
const orange: RgbColor = { r: 255, g: 165, b: 0 };

export const styledOutput = style.red`Danger`;
export const fromString: StyleMatch = style.red("^Danger:");
export const fromRegExp: StyleMatch = style.red(/^Danger:/is);
export const fromFriendlyPattern: StyleMatch = style.red(pattern.contains`Danger: {message}`);
export const refined: StyleMatch = style.red({ bg: "black" }).bold(/^Danger:/);
export const noOp: StyleMatch = style({})(/^ordinary$/);

export const ranged: StyleMatchBuilder = style.fg.range(crimson, orange);
export const overwrittenRange: StyleMatchBuilder = ranged.fg("red");
export const combinedRange = ranged.bg.range(orange, crimson).bold.underline;
export const leaves: readonly TriggerPattern[] = [
  style.italic,
  fromString,
  combinedRange(pattern.contains`Warning: {message}`),
];
export const grouped: TriggerPatterns = {
  patterns: leaves,
  rawPatterns: [/\x1b\[/],
  antiPatterns: [style.faint(/harmless/), style.red],
};

createTrigger(fromString, "flee");
createTrigger(style.italic, "look");
createTrigger(combinedRange, "score");
createTrigger(grouped, "look");
createTrigger(overwrittenRange(/^Danger:/), "flee");

createTriggers({
  warning: {
    patterns: [style.yellow(/^Warning:/), style.red(/^Danger:/)],
    antiPatterns: [style.faint(/harmless/)],
    script: "look",
  },
  recovery: {
    patterns: [style.green(pattern.startsWith`You recover`)],
    script: ({ 0: text }) => text,
  },
});
"#
            .to_string(),
        );

        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("compile a consumer of every styled-trigger overload");
        assert!(
            out.diagnostics.is_empty(),
            "the supported styled-trigger surface must compile:\n{:#?}",
            out.diagnostics
        );
    }

    /// Matcher-only builders and decorated leaves must not leak into the older output, alias,
    /// raw-pattern, or persisted-trigger surfaces. Each source is compiled independently so one
    /// expected diagnostic cannot mask a missing exclusion elsewhere.
    #[test]
    fn styled_trigger_exclusions_fail_to_compile() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );

        for (name, source) in [
            (
                "styled-alias.ts",
                r#"import { createAlias, style } from "smudgy:core";
createAlias(style.red(/danger/), "flee");
"#,
            ),
            (
                "styled-raw.ts",
                r#"import { createTrigger, style } from "smudgy:core";
createTrigger({ rawPatterns: [style.red(/danger/)] }, "flee");
"#,
            ),
            (
                "range-output.ts",
                r#"import { style } from "smudgy:core";
const a = { r: 0, g: 0, b: 0 };
style.fg.range(a, a)`not output`;
"#,
            ),
            (
                "range-echo.ts",
                r#"import { echo, style } from "smudgy:core";
const a = { r: 0, g: 0, b: 0 };
echo(style.fg.range(a, a));
"#,
            ),
            (
                "range-line-option.ts",
                r#"import { line, style } from "smudgy:core";
const a = { r: 0, g: 0, b: 0 };
line.highlight("danger", style.fg.range(a, a));
"#,
            ),
            (
                "whole-pattern-object.ts",
                r#"import { style } from "smudgy:core";
style.red({ patterns: [/danger/] });
"#,
            ),
            (
                "nested-style-match.ts",
                r#"import { style } from "smudgy:core";
style.red(style.blue(/danger/));
"#,
            ),
            (
                "non-body-trigger-argument.ts",
                r#"import { createTrigger, style } from "smudgy:core";
createTrigger(style.red(/danger/), 42);
"#,
            ),
            (
                "persisted-style-match.ts",
                r#"import { style, userAutomations } from "smudgy:core";
userAutomations.triggers.save("danger", { patterns: [style.red(/danger/)] });
"#,
            ),
        ] {
            let mut sources = BTreeMap::new();
            sources.insert(name.to_string(), source.to_string());
            let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
                .expect("the generator itself must not crash on a type error");
            assert!(
                !out.diagnostics.is_empty(),
                "{name} must fail the styled-trigger type boundary"
            );
        }
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
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );

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

    /// Drift guard for the MAP types: the mapper runtime impl (`mapper.ts`) and the
    /// author-facing contract (`smudgy-mapper.d.ts`) are separate files, so this compiles them
    /// together and asserts (1) the impl is valid TypeScript on its own (ops are an `any` FFI
    /// boundary via the `@ts-ignore`d `ext:core/ops` import) and (2) the impl's `mapper` object,
    /// `Area` constructor, and `Area`/`Room`/`Exit` shapes are assignable to the published module
    /// value and global ambient map types — so the runtime cannot silently expose map types
    /// incompatible with what the declarations promise authors (the regression that left external
    /// packages' `Room`/`Area`/… usage stranded). The impl exposes these via the type-only `*Impl`
    /// exports at the end of `mapper.ts`.
    #[test]
    fn mapper_ts_impl_conforms_to_contract() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        // The contract declares `Mapper`/`Area`/`Room`/`Exit`/`AreaId`/… as global ambient types,
        // while smudgy:core publishes the Area runtime constructor.
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());

        let mut sources = BTreeMap::new();
        sources.insert("impl.ts".to_string(), SMUDGY_MAPPER_TS.to_string());
        sources.insert(
            "check.ts".to_string(),
            "import { Area } from \"smudgy:core\";\n\
             import type { MapperImpl, AreaConstructorImpl, AreaImpl, RoomImpl, ExitImpl } from \"./impl.ts\";\n\
             declare const m: MapperImpl;\n\
             declare const areaConstructor: AreaConstructorImpl;\n\
             declare const a: AreaImpl;\n\
             declare const r: RoomImpl;\n\
             declare const e: ExitImpl;\n\
             // The runtime impl must fulfill the published global map-type contract.\n\
             export const __mapper: Mapper = m;\n\
             export const __areaConstructor: typeof Area = areaConstructor;\n\
             export const __area: Area = a;\n\
             export const __room: Room = r;\n\
             export const __exit: Exit = e;\n\
             export const __instanceof: boolean = a instanceof Area;\n"
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

    /// The compatibility catalog is deliberately finite: the `ephemeral`
    /// creation flag and the `isEphemeral` read. These assertions do three
    /// jobs together: old scripts still type-check during 0.5.x, every
    /// compatibility member carries an editor-visible deprecation, and the
    /// test itself blocks the first 0.6 build until the shims are removed.
    /// Creating with no storage choice at all is NOT in the catalog: it is
    /// the supported default (durable, cloud when signed in, local
    /// otherwise) and stays past 0.6.
    #[test]
    fn map_storage_compatibility_is_deprecated_and_expires_in_0_6() {
        use std::collections::BTreeMap;

        const DEPRECATION: &str = "@deprecated Supported through Smudgy 0.5.x; removed in 0.6.0.";

        assert_eq!(
            smudgy_cloud::MAP_STORAGE_COMPATIBILITY_LAST_RELEASE,
            "0.5.x"
        );
        assert_eq!(
            smudgy_cloud::MAP_STORAGE_COMPATIBILITY_REMOVAL_VERSION,
            "0.6.0"
        );
        let running = semver::Version::parse(env!("CARGO_PKG_VERSION"))
            .expect("Cargo package versions are valid semver");
        assert!(
            (running.major, running.minor) < (0, 6),
            "remove the mapper's `ephemeral` creation flag and `isEphemeral` \
             before building the 0.6 release line"
        );

        assert_eq!(
            SMUDGY_MAPPER_DTS.matches(DEPRECATION).count(),
            2,
            "the compatibility catalog is exactly: CreateAreaOptions.ephemeral \
             and Area.isEphemeral"
        );
        assert_eq!(
            SMUDGY_MAPPER_TS.matches(DEPRECATION).count(),
            2,
            "the runtime implementation must mark its ephemeral option and getter"
        );
        let nukefire_mapper = include_str!("../../../packages/nukefire-mapper/mapper.ts");
        assert_eq!(
            nukefire_mapper.matches(DEPRECATION).count(),
            1,
            "the first-party NukeFire mapper's ephemeral option is part of the same finite window"
        );
        // Arctic deliberately omits `storage` so signed-in sessions prefer cloud
        // and signed-out sessions fall back to local. That is the supported
        // default, not part of this compatibility window, so no first-party
        // source assertion covers it.

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );
        let mut sources = BTreeMap::new();
        sources.insert(
            "compatibility-consumer.ts".to_string(),
            "import { mapper } from \"smudgy:core\";\n\
             declare const area: Area;\n\
             // The storage-less forms are the supported default, not compatibility.\n\
             export const implicitDefault = mapper.createArea(\"implicit default\");\n\
             export const implicitEmpty = mapper.createArea(\"implicit empty\", {});\n\
             export const oldSession = mapper.createArea(\"old session\", { ephemeral: true });\n\
             export const oldPredicate: boolean = area.isEphemeral;\n\
             export const canonical = mapper.createArea(\"canonical\", { storage: \"local\" });\n"
                .to_string(),
        );
        let out = smudgy_script::dts::generate_declarations(&sources, &ambient)
            .expect("compile supported, compatibility, and canonical mapper creation forms");
        assert!(
            out.diagnostics.is_empty(),
            "a supported creation form or a 0.5 compatibility form stopped type-checking:\n{:#?}",
            out.diagnostics
        );
    }

    /// Rust↔TS enum drift guard: every serde variant name of the map enums must
    /// appear in both the published contract and runtime implementation.
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
        assert_covered(&smudgy_cloud::ExitDirection::ALL, "ExitDirection");
        assert_covered(&smudgy_cloud::ShapeType::ALL, "ShapeType");
        assert_covered(
            &smudgy_cloud::HorizontalAlignment::ALL,
            "HorizontalAlignment",
        );
        assert_covered(&smudgy_cloud::VerticalAlignment::ALL, "VerticalAlignment");
        assert_covered(&smudgy_cloud::RoomSide::ALL, "RoomSide");
        assert_covered(
            &[
                smudgy_cloud::PortMode::AutoPinned,
                smudgy_cloud::PortMode::Manual,
            ],
            "PortMode",
        );
        assert_covered(
            &[
                smudgy_cloud::ConnectionKind::Internal,
                smudgy_cloud::ConnectionKind::SelfLoop,
                smudgy_cloud::ConnectionKind::Dangling,
                smudgy_cloud::ConnectionKind::External,
                smudgy_cloud::ConnectionKind::CrossLevel,
            ],
            "ConnectionKind",
        );
        assert_covered(&smudgy_cloud::ConnectionRouting::ALL, "ConnectionRouting");
        assert_covered(&smudgy_cloud::SegmentShape::ALL, "SegmentShape");
        assert_covered(&smudgy_cloud::CornerStyle::ALL, "CornerStyle");
        assert_covered(&smudgy_cloud::ConnectionDash::ALL, "ConnectionDash");
    }

    /// Coverage guard for EXTERNAL packages: compile a consumer that reaches the map the way
    /// installed `smudgy://` package scripts do — `Room`/`Area`/`Exit`/`ExitId`/`RoomNumber`/
    /// `AreaId` as AMBIENT GLOBALS (no type imports), with the runtime `mapper`/`Area` values imported
    /// from `smudgy:core` — against the shipped typings. A clean compile proves the map types stay
    /// ambient while the two values use the module surface; a regression here is the "map types no
    /// longer available" breakage.
    #[test]
    fn external_package_map_surface_is_typed() {
        use std::collections::BTreeMap;

        let mut ambient = BTreeMap::new();
        ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
        ambient.insert(
            "smudgy-mapper.d.ts".to_string(),
            SMUDGY_MAPPER_DTS.to_string(),
        );

        let mut sources = BTreeMap::new();
        sources.insert(
            "consumer.ts".to_string(),
            // Room/Area/Exit/ExitId/RoomNumber/AreaId stay global ambient types; mapper and the
            // Area constructor are explicit runtime imports.
            r##"
            import { mapper, Area } from "smudgy:core";

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
              const uuid: string = area.uuid;
              const name: string = area.name;
              const nums: RoomNumber[] = area.room_numbers;
              const next: RoomNumber = area.next_room_number;
              const r: Room | undefined = area.room(1);
              const p: string | undefined = area.data("notes");
              void id; void uuid; void name; void nums; void next; void r; void p;
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
              const runtimeCheck: boolean = newArea instanceof Area;
              const newRoom: RoomNumber = await mapper.createRoom(room.area_id, { title: "x" });
              const batchIds: OperationId[] = await mapper.mutateArea(room.area_id, async (mutation) => {
                const batchedRoom: RoomNumber = await mutation.createRoom({ title: "batch" });
                await mutation.setRoomProperty(batchedRoom, "terrain", "city");
                await mutation.createRoomExit(batchedRoom, { from_direction: "South" });
              }, { description: "typed batch" });
              const exitId: ExitId = await mapper.createRoomExit(room.area_id, room.room_number, { from_direction: "North" });
              const updateId: OperationId | null = await mapper.setRoomExit(room.area_id, room.room_number, exitId, { command: "enter hole" });
              const mergeId: OperationId | null = await mapper.mergeRooms(room.area_id, room.room_number, room.room_number + 1);
              await mapper.deleteRoomExit(room.area_id, room.room_number, exitId);
              await mapper.deleteRoom(room.area_id, room.room_number);
              mapper.setCurrentLocation(room.area_id, room.room_number);
              await mapper.setRoomProperty(room.area_id, room.room_number, "k", "v");
              await mapper.setAreaProperty(room.area_id, "k", "v");
              await mapper.addRoomTag(room.area_id, room.room_number, "INN");
              await mapper.removeRoomTag(room.area_id, room.room_number, "INN");
              await mapper.setRoomColor(room.area_id, room.room_number, "#fff");
              await mapper.setRoomX(room.area_id, room.room_number, 1);
              await mapper.setRoomY(room.area_id, room.room_number, 1);
              await mapper.setRoomLevel(room.area_id, room.room_number, 1);
              await mapper.setRoomTitle(room.area_id, room.room_number, "t");
              await mapper.setRoomDescription(room.area_id, room.room_number, "d");
              await mapper.renameArea(room.area_id, "n");
              void areas; void a; void path; void near; void near1; void list; void list2; void newArea; void runtimeCheck; void newRoom; void batchIds; void updateId; void mergeId;
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
        assert!(
            dir.join(".smudgy/node-types/@types/node/events.d.ts")
                .is_file()
        );
        assert!(dir.join(".smudgy/.runtime-types-version").is_file());

        // The base tsconfig wires @types/node via types + typeRoots.
        let base = fs::read_to_string(dir.join(".smudgy/tsconfig.base.json")).unwrap();
        assert!(base.contains("\"node\""), "types: [node] missing:\n{base}");
        assert!(
            base.contains("./node-types/@types"),
            "typeRoots missing:\n{base}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn in_memory_language_service_types_are_static_complete_and_capability_safe() {
        use std::collections::HashSet;

        let files = embedded_language_service_types();
        let paths = files
            .iter()
            .map(|file| file.virtual_path.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(paths.len(), files.len(), "virtual paths must be unique");
        assert!(paths.contains("smudgy/smudgy-core.d.ts"));
        assert!(paths.contains("lib.deno.ns.d.ts"));
        assert!(paths.contains("node-types/@types/node/events.d.ts"));
        assert!(paths.contains("node-types/@types/node/web-globals/timers.d.ts"));
        assert!(
            files.iter().any(|file| {
                file.virtual_path == "node-types/@types/node/ts5.6/index.d.ts" && file.is_root
            }),
            "the embedded TypeScript 5.6 compiler needs the matching Node entry point"
        );
        assert!(
            files
                .iter()
                .filter(|file| file.virtual_path.starts_with("lib.deno"))
                .all(|file| file.is_root),
            "the vendored Deno references were stripped, so each declaration is a root"
        );
        assert!(
            !files.iter().any(|file| {
                file.virtual_path.contains("web-audio")
                    || file.virtual_path.contains("smudgy-inline")
                    || file.virtual_path.contains("installed-events")
                    || file.virtual_path.contains("interop-handles")
            }),
            "scoped, capability-, and package-dependent declarations need explicit sources"
        );
        assert!(
            !paths.contains("node-types/@types/node/index.d.ts")
                && !paths.contains("node-types/@types/node/globals.typedarray.d.ts")
                && !paths.contains("node-types/@types/node/buffer.buffer.d.ts")
                && !paths
                    .iter()
                    .any(|path| path.starts_with("node-types/@types/node/ts5.7/")),
            "only the TypeScript 5.6-compatible Node declaration entry may be addressable"
        );
        assert!(
            paths.iter().all(|path| {
                !path.starts_with("node-types/@types/node/web-globals/")
                    || *path == "node-types/@types/node/web-globals/timers.d.ts"
            }),
            "Deno declarations own overlapping web globals; only hybrid timers remain"
        );
        assert!(
            files
                .iter()
                .filter(|file| file.virtual_path.starts_with("lib.deno"))
                .all(|file| {
                    ![
                        "Uint8Array<ArrayBuffer>",
                        "Uint8Array<ArrayBufferLike>",
                        "Uint8ClampedArray<ArrayBuffer>",
                        "Float16Array<ArrayBuffer>",
                        "Float32Array<ArrayBuffer>",
                        "Float64Array<ArrayBuffer>",
                        "ArrayBufferView<ArrayBuffer>",
                    ]
                    .iter()
                    .any(|unsupported| file.contents.contains(unsupported))
                }),
            "the Deno snapshot must remain valid under the embedded TypeScript 5.6 compiler"
        );
        assert_eq!(
            files.iter().filter(|file| file.is_root).count(),
            22,
            "five Smudgy roots + sixteen Deno roots + the TypeScript 5.6 Node root"
        );
    }

    /// The in-process language service driven the way the automations window drives it:
    /// spawned on the embedded declarations, commands down one channel, events drained by
    /// polling.
    mod service_harness {
        use std::thread;
        use std::time::{Duration, Instant};

        use smudgy_script::language_service::{
            AcknowledgedState, AnalysisContextId, AutomationKind, ClientId, Command, Diagnostic,
            DiskRevision, DocumentDescriptor, DocumentId, DocumentKey, DocumentKind, DocumentRef,
            DocumentRequest, DocumentResultIdentity, DocumentVersion, Event, EventEnvelope,
            GraphGeneration, Language, LanguageServiceLibrary, OpenDocument, OpenProject,
            ProjectId, ProjectScope, ProjectSource, RefreshProject, RequestId,
        };
        use smudgy_script::language_service_worker::{LanguageServiceClient, LanguageServiceHost};

        /// Spawns the service on the embedded declarations and opens one project.
        pub(super) fn open_project(
            client_id: u64,
            project_id: u64,
        ) -> (LanguageServiceHost, LanguageServiceClient, ProjectScope) {
            let libraries = crate::models::script_typings::embedded_language_service_types()
                .into_iter()
                .map(|file| LanguageServiceLibrary {
                    file_name: file.virtual_path,
                    text: file.contents.into(),
                    is_root: file.is_root,
                })
                .collect();
            let mut host = LanguageServiceHost::try_spawn_with_libraries(libraries)
                .expect("spawn language service with Smudgy's embedded declarations");
            let client = host.client();
            let project = ProjectScope {
                client_id: wire::<ClientId>(client_id),
                project_id: wire::<ProjectId>(project_id),
            };
            client
                .send(Command::OpenProject(OpenProject { project }))
                .expect("queue project open");
            wait_for(&mut host, |event| {
                matches!(
                    event,
                    Event::StateAcknowledged(AcknowledgedState::ProjectOpened(state))
                        if state.project == project
                )
            });
            (host, client, project)
        }

        /// Installs `bridge` as the project's inline context at graph generation `generation`
        /// (an opened project sits at 1, so a first refresh is 2) and waits for the ack.
        pub(super) fn install_inline_bridge(
            host: &mut LanguageServiceHost,
            client: &LanguageServiceClient,
            project: ProjectScope,
            generation: u64,
            bridge: String,
        ) {
            let graph_generation = wire::<GraphGeneration>(generation);
            client
                .send(Command::RefreshProject(RefreshProject {
                    project,
                    graph_generation,
                    sources: vec![ProjectSource {
                        document_id: DocumentId::try_from([76; 16])
                            .expect("non-nil inline-context document ID"),
                        uri: "smudgy-project:///inline/context.d.ts".to_owned(),
                        language: Language::TypeScript,
                        kind: DocumentKind::Generated,
                        text: bridge,
                    }],
                }))
                .expect("queue inline-context refresh");
            wait_for(host, |event| {
                matches!(
                    event,
                    Event::StateAcknowledged(AcknowledgedState::ProjectRefreshed(state))
                        if state.project == project && state.graph_generation == graph_generation
                )
            });
        }

        pub(super) fn wire<T>(value: u64) -> T
        where
            T: TryFrom<u64>,
            T::Error: std::fmt::Debug,
        {
            T::try_from(value).expect("valid test wire value")
        }

        pub(super) fn wait_for(
            host: &mut LanguageServiceHost,
            predicate: impl Fn(&Event) -> bool,
        ) -> EventEnvelope {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                for envelope in host.drain_events() {
                    if let Event::RequestFailed(failure) = &envelope.event {
                        panic!("language-service request failed: {failure:?}");
                    }
                    if predicate(&envelope.event) {
                        return envelope;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "language-service event timed out"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }

        pub(super) fn descriptor(
            project: ProjectScope,
            document_id: DocumentId,
            uri: &str,
            kind: DocumentKind,
            analysis_context: u64,
        ) -> DocumentDescriptor {
            DocumentDescriptor {
                document: DocumentRef {
                    key: DocumentKey {
                        project,
                        document_id,
                    },
                    view: None,
                    version: wire::<DocumentVersion>(1),
                },
                uri: uri.to_owned(),
                language: Language::TypeScript,
                kind,
                analysis_context: wire::<AnalysisContextId>(analysis_context),
                disk_revision: Some(wire::<DiskRevision>(1)),
            }
        }

        /// Opens `text` as an inline alias body (ids and the request derived from `seed`,
        /// distinct per call) and returns its diagnostics.
        pub(super) fn inline_diagnostics(
            host: &mut LanguageServiceHost,
            client: &LanguageServiceClient,
            project: ProjectScope,
            seed: u8,
            uri: &str,
            text: &str,
        ) -> Vec<Diagnostic> {
            let document_id = DocumentId::try_from([seed; 16]).expect("non-nil document ID");
            client
                .send(Command::OpenDocument(OpenDocument {
                    descriptor: descriptor(
                        project,
                        document_id,
                        uri,
                        DocumentKind::InlineAutomation {
                            automation_kind: AutomationKind::Alias,
                        },
                        u64::from(seed),
                    ),
                    text: text.to_owned(),
                }))
                .expect("queue inline document open");
            let opened = wait_for(host, |event| {
                matches!(
                    event,
                    Event::StateAcknowledged(AcknowledgedState::DocumentOpened(state))
                        if state.document.key.document_id == document_id
                )
            });
            let Event::StateAcknowledged(AcknowledgedState::DocumentOpened(state)) = opened.event
            else {
                unreachable!();
            };
            let request_id = wire::<RequestId>(u64::from(seed));
            client
                .send(Command::RequestDiagnostics(DocumentRequest {
                    identity: DocumentResultIdentity { state, request_id },
                }))
                .expect("queue inline diagnostics");
            let diagnostics = wait_for(host, |event| {
                matches!(
                    event,
                    Event::Diagnostics(result) if result.identity.request_id == request_id
                )
            });
            let Event::Diagnostics(diagnostics) = diagnostics.event else {
                unreachable!();
            };
            diagnostics.result.items
        }
    }

    #[test]
    fn in_memory_language_service_bundle_types_runtime_and_inline_surfaces() {
        use smudgy_script::language_service::{
            AcknowledgedState, AutomationKind, ClientId, Command, DiagnosticCode,
            DiagnosticSeverity, DocumentId, DocumentKind, DocumentResultIdentity, Event,
            GraphGeneration, Language, LanguageServiceLibrary, OpenDocument, OpenProject,
            ProjectId, ProjectScope, ProjectSource, RefreshProject, RequestId,
        };
        use smudgy_script::language_service_worker::LanguageServiceHost;

        use service_harness::{descriptor, wait_for, wire};

        let libraries = embedded_language_service_types()
            .into_iter()
            .map(|file| LanguageServiceLibrary {
                file_name: file.virtual_path,
                text: file.contents.into(),
                is_root: file.is_root,
            })
            .collect();
        let mut host = LanguageServiceHost::try_spawn_with_libraries(libraries)
            .expect("spawn language service with Smudgy's embedded declarations");
        let client = host.client();
        let project = ProjectScope {
            client_id: wire::<ClientId>(71),
            project_id: wire::<ProjectId>(72),
        };

        client
            .send(Command::OpenProject(OpenProject { project }))
            .expect("queue project open");
        wait_for(&mut host, |event| {
            matches!(
                event,
                Event::StateAcknowledged(AcknowledgedState::ProjectOpened(state))
                    if state.project == project
            )
        });

        let module_id = DocumentId::try_from([73; 16]).expect("non-nil module document ID");
        let module_descriptor = descriptor(
            project,
            module_id,
            "smudgy-project:///modules/runtime-types.ts",
            DocumentKind::StandaloneModule,
            74,
        );
        client
            .send(Command::OpenDocument(OpenDocument {
                descriptor: module_descriptor,
                text: concat!(
                    "/// <reference lib=\"deno.ns\" />\n",
                    "/// <reference types=\"node\" />\n",
                    "import { echo } from \"smudgy:core\";\n",
                    "import { join } from \"node:path\";\n",
                    "type IsAny<T> = 0 extends (1 & T) ? true : false;\n",
                    "const denoReadIsAny: IsAny<Awaited<ReturnType<typeof Deno.readFile>>> = false;\n",
                    "const denoFetchIsAny: IsAny<Awaited<ReturnType<typeof fetch>>> = false;\n",
                    "void denoReadIsAny; void denoFetchIsAny;\n",
                    "const timeoutHandle: number = setTimeout(() => {}, 1);\n",
                    "clearTimeout(timeoutHandle);\n",
                    "setTimeout(() => {}, 1).ref();\n",
                    "const immediateHandle = setImmediate(() => {});\n",
                    "clearImmediate(immediateHandle);\n",
                    "const worker = new Worker(\"data:text/javascript,export {};\", { type: \"module\" });\n",
                    "const eventTarget: EventTarget = worker;\n",
                    "eventTarget.dispatchEvent(new Event(\"probe\"));\n",
                    "worker.addEventListener(\"message\", (event) => { event.data.toString(); });\n",
                    "echo(join(Deno.cwd(), \"logs\"));\n",
                    "const mismatch: number = \"wrong\";\n",
                    "void mismatch;\n",
                )
                .to_owned(),
            }))
            .expect("queue runtime-typing document open");
        let opened = wait_for(&mut host, |event| {
            matches!(
                event,
                Event::StateAcknowledged(AcknowledgedState::DocumentOpened(state))
                    if state.document.key.document_id == module_id
            )
        });
        let Event::StateAcknowledged(AcknowledgedState::DocumentOpened(module_state)) =
            opened.event
        else {
            unreachable!();
        };
        let module_request = wire::<RequestId>(75);
        client
            .send(Command::RequestDiagnostics(
                smudgy_script::language_service::DocumentRequest {
                    identity: DocumentResultIdentity {
                        state: module_state,
                        request_id: module_request,
                    },
                },
            ))
            .expect("queue runtime-typing diagnostics");
        let diagnostics = wait_for(&mut host, |event| {
            matches!(
                event,
                Event::Diagnostics(result) if result.identity.request_id == module_request
            )
        });
        let Event::Diagnostics(diagnostics) = diagnostics.event else {
            unreachable!();
        };
        assert!(
            !diagnostics.result.items.iter().any(|item| {
                matches!(
                    item.code,
                    Some(DiagnosticCode::Number(
                        2304 | 2307 | 2552 | 2580 | 2591 | 2688 | 2726 | 2792
                    ))
                ) || item.message.contains("Cannot find name")
                    || item.message.contains("Cannot find module")
            }),
            "smudgy:core, Deno, and node:path must resolve: {:?}",
            diagnostics.result.items
        );
        assert!(
            diagnostics
                .result
                .items
                .iter()
                .any(|item| item.code == Some(DiagnosticCode::Number(2322))),
            "the declaration bundle must preserve ordinary semantic checking"
        );
        assert!(
            diagnostics
                .result
                .items
                .iter()
                .any(|item| item.code == Some(DiagnosticCode::Number(2339))),
            "Deno timer handles must not expose Node's ref() API"
        );
        assert!(
            diagnostics
                .result
                .items
                .iter()
                .any(|item| item.code == Some(DiagnosticCode::Number(18046))),
            "Worker message payloads must require narrowing before use"
        );
        let errors = diagnostics
            .result
            .items
            .iter()
            .filter(|item| item.severity == DiagnosticSeverity::Error)
            .collect::<Vec<_>>();
        assert_eq!(
            errors.len(),
            3,
            "Deno APIs, hybrid globals, and Worker/EventTarget assignability must otherwise type-check: {:?}",
            diagnostics.result.items
        );

        let graph_generation = wire::<GraphGeneration>(2);
        client
            .send(Command::RefreshProject(RefreshProject {
                project,
                graph_generation,
                sources: vec![ProjectSource {
                    document_id: DocumentId::try_from([76; 16])
                        .expect("non-nil inline-context document ID"),
                    uri: "smudgy-project:///inline/context.d.ts".to_owned(),
                    language: Language::TypeScript,
                    kind: DocumentKind::Generated,
                    text: language_service_inline_bridge().to_owned(),
                }],
            }))
            .expect("queue inline-context refresh");
        wait_for(&mut host, |event| {
            matches!(
                event,
                Event::StateAcknowledged(AcknowledgedState::ProjectRefreshed(state))
                    if state.project == project && state.graph_generation == graph_generation
            )
        });

        let inline_id = DocumentId::try_from([77; 16]).expect("non-nil inline document ID");
        let inline_descriptor = descriptor(
            project,
            inline_id,
            "smudgy-inline:///aliases/runtime-types.ts",
            DocumentKind::InlineAutomation {
                automation_kind: AutomationKind::Alias,
            },
            78,
        );
        client
            .send(Command::OpenDocument(OpenDocument {
                descriptor: inline_descriptor,
                text: concat!(
                    "const id = \"shadow\";\n",
                    "const line = \"shadow\";\n",
                    "const input = \"shadow\";\n",
                    "const matches = [\"\", \"capture\"];\n",
                    "send(`${id}:${line}:${input}:${matches[1]}`);\n",
                    "void import(\"smudgy:core\").then(({ echo }) => echo(line));\n",
                    "void import(\"node:path\").then(({ join }) => join(input, \"logs\"));\n",
                )
                .to_owned(),
            }))
            .expect("queue inline document open");
        let opened = wait_for(&mut host, |event| {
            matches!(
                event,
                Event::StateAcknowledged(AcknowledgedState::DocumentOpened(state))
                    if state.document.key.document_id == inline_id
            )
        });
        let Event::StateAcknowledged(AcknowledgedState::DocumentOpened(inline_state)) =
            opened.event
        else {
            unreachable!();
        };
        let inline_request = wire::<RequestId>(79);
        client
            .send(Command::RequestDiagnostics(
                smudgy_script::language_service::DocumentRequest {
                    identity: DocumentResultIdentity {
                        state: inline_state,
                        request_id: inline_request,
                    },
                },
            ))
            .expect("queue inline diagnostics");
        let diagnostics = wait_for(&mut host, |event| {
            matches!(
                event,
                Event::Diagnostics(result) if result.identity.request_id == inline_request
            )
        });
        let Event::Diagnostics(diagnostics) = diagnostics.event else {
            unreachable!();
        };
        assert!(
            !diagnostics
                .result
                .items
                .iter()
                .any(|item| item.code == Some(DiagnosticCode::Number(2451))),
            "inline runtime globals must remain locally shadowable"
        );
        assert!(
            diagnostics.result.items.is_empty(),
            "the scoped inline bridge and supported dynamic imports must type-check: {:?}",
            diagnostics.result.items
        );

        host.shutdown()
            .expect("language-service worker must shut down cleanly");
    }

    #[test]
    fn inline_language_service_bridge_covers_every_smudgy_api_member() {
        use std::collections::BTreeSet;

        let interface = SMUDGY_CORE_DTS
            .split_once("export interface SmudgyApi {")
            .expect("SmudgyApi interface")
            .1
            .split_once("\n  }")
            .expect("SmudgyApi interface end")
            .0;
        let api_members = interface
            .lines()
            .filter_map(|line| {
                let member = line.trim().strip_prefix("readonly ").unwrap_or(line.trim());
                if member.is_empty() || member.starts_with(['/', '*']) {
                    return None;
                }
                member
                    .split(['(', ':'])
                    .next()
                    .filter(|name| !name.is_empty())
            })
            .collect::<BTreeSet<_>>();
        let mut bridge_members = language_service_inline_bridge()
            .lines()
            .filter_map(|line| line.trim().strip_prefix("const "))
            .filter_map(|declaration| declaration.split_once(':').map(|(name, _)| name))
            .collect::<BTreeSet<_>>();
        assert!(bridge_members.remove("matches"));
        // Like `matches`, `outer` is a per-fire global, not an API member.
        assert!(bridge_members.remove("outer"));
        assert_eq!(bridge_members, api_members);
    }

    fn state_exposure(
        producer: &str,
        handle: Option<&str>,
        name_override: Option<&str>,
        paths: &[&str],
    ) -> StateExposure {
        StateExposure {
            producer: producer.to_owned(),
            handle: handle.map(str::to_owned),
            name_override: name_override.map(str::to_owned),
            paths: paths.iter().map(ToString::to_string).collect(),
        }
    }

    /// The per-automation bridge: the fixed text while nothing usable is exposed; otherwise
    /// the shadowed globals go, and every exposed name is declared once, platform trees
    /// typed along their bound paths (covered paths dropped, shared prefixes unified, keys
    /// outside the identifier grammar quoted), handles as `any`.
    #[test]
    fn inline_bridge_for_exposures_replaces_shadowed_globals_and_declares_names() {
        let fixed = language_service_inline_bridge_for(&[]);
        assert!(matches!(fixed, Cow::Borrowed(_)));
        assert_eq!(fixed, language_service_inline_bridge());
        let unusable = language_service_inline_bridge_for(&[
            state_exposure("user", None, None, &[""]),
            state_exposure("gmcp", None, None, &[]),
            state_exposure("nope://x", None, None, &[""]),
        ]);
        assert_eq!(unusable, language_service_inline_bridge());

        let bridge = language_service_inline_bridge_for(&[
            state_exposure(
                "gmcp",
                None,
                None,
                &[
                    "Char.Vitals",
                    "char.Status",
                    "Room.Info.name",
                    "Char.Vitals.hp",
                    "[\"Some-Pkg\"].Msg",
                ],
            ),
            state_exposure("user", Some("foo"), Some("stats"), &["bar"]),
            state_exposure(
                "smudgy://kapusniak/arctic-prompt",
                Some("prompt"),
                None,
                &["groupies[\"Mr. Foo\"].hp"],
            ),
            state_exposure("mssp", None, None, &[""]),
            state_exposure("msdp", None, Some("vars"), &["ROOM.VNUM", "ROOM_NAME"]),
            // A folded duplicate of `gmcp`, an unresolvable entry: both left out.
            state_exposure("user", Some("GMCP"), None, &[""]),
            state_exposure("nope://x", None, None, &[""]),
            // A name that takes the per-fire `matches` global.
            state_exposure("user", Some("m"), Some("matches"), &[""]),
        ]);
        for shadowed in [
            "const gmcp: SmudgyApi[\"gmcp\"];",
            "const vars: SmudgyUserVars;",
            "const matches: Matches;",
        ] {
            assert!(!bridge.contains(shadowed), "{shadowed} must go:\n{bridge}");
        }
        for kept in [
            "import type { Matches, SmudgyApi } from \"smudgy:core\";\n\
             import type { GmcpTree, MsdpTree, MsspVariables } from \"smudgy:core\";\n",
            "interface SmudgyUserVars {",
            "  const send: SmudgyApi[\"send\"];\n",
            "  const id: SmudgyApi[\"id\"];\n",
            STATE_HOP_TYPE,
            "  const gmcp: { Char: { \
             Status: SmudgyStateHop<SmudgyStateHop<GmcpTree, \"Char\">, \"Status\"> | undefined; \
             Vitals: SmudgyStateHop<SmudgyStateHop<GmcpTree, \"Char\">, \"Vitals\"> | undefined }; \
             Room: { Info: { name: SmudgyStateHop<SmudgyStateHop<SmudgyStateHop<GmcpTree, \
             \"Room\">, \"Info\">, \"name\"> | undefined } }; \
             \"Some-Pkg\": { Msg: SmudgyStateHop<SmudgyStateHop<GmcpTree, \"Some-Pkg\">, \"Msg\"> \
             | undefined } };\n",
            "  const stats: any;\n",
            "  const prompt: any;\n",
            "  const mssp: MsspVariables | undefined;\n",
            "  const vars: { ROOM: { VNUM: SmudgyStateHop<SmudgyStateHop<MsdpTree, \"ROOM\">, \
             \"VNUM\"> | undefined }; ROOM_NAME: SmudgyStateHop<MsdpTree, \"ROOM_NAME\"> | \
             undefined };\n",
            "  const matches: any;\n",
        ] {
            assert!(bridge.contains(kept), "{kept} missing:\n{bridge}");
        }
        assert!(!bridge.contains("const GMCP"));
        assert_eq!(bridge.matches("declare global {").count(), 2);
        assert!(bridge.ends_with("}\n\nexport {};\n"), "{bridge}");

        // A root-only platform exposure needs no hop helper; a handle-only list no tree import.
        let root = language_service_inline_bridge_for(&[state_exposure("gmcp", None, None, &[""])]);
        assert!(root.contains("  const gmcp: GmcpTree | undefined;\n"));
        assert!(!root.contains("SmudgyStateHop"));
        assert!(root.contains("import type { GmcpTree } from \"smudgy:core\";\n"));
        let handle =
            language_service_inline_bridge_for(&[state_exposure("user", Some("foo"), None, &[""])]);
        assert!(handle.contains("  const foo: any;\n"));
        assert_eq!(handle.matches("import type").count(), 1);
    }

    /// A name JavaScript cannot bind is left undeclared, so the bridge stays parse-clean: the
    /// other exposures are declared as before, a contextual keyword is an ordinary name, and a
    /// list with nothing else usable is the fixed text.
    #[test]
    fn inline_bridge_leaves_out_names_javascript_cannot_bind() {
        let bridge = language_service_inline_bridge_for(&[
            state_exposure("user", Some("if"), None, &[""]),
            state_exposure("user", Some("kind"), Some("class"), &["hp"]),
            state_exposure("gmcp", None, Some("let"), &["Char.Vitals"]),
            state_exposure("user", Some("e"), Some("eval"), &[""]),
            state_exposure("user", Some("a"), Some("arguments"), &[""]),
            state_exposure("user", Some("i"), Some("in"), &[""]),
            state_exposure("user", Some("stats"), None, &["bar"]),
        ]);
        for unbindable in ["if", "class", "let", "eval", "arguments", "in"] {
            assert!(
                !bridge.contains(&format!("const {unbindable}:")),
                "{unbindable} must stay undeclared:\n{bridge}"
            );
        }
        // A handle that is itself a reserved word defaults to its `_` form and is declared.
        assert!(bridge.contains("  const if_: any;\n"), "{bridge}");
        assert!(bridge.contains("  const stats: any;\n"), "{bridge}");
        assert!(
            bridge.contains("const gmcp: SmudgyApi[\"gmcp\"];"),
            "a `gmcp` exposure under another name shadows nothing:\n{bridge}"
        );
        assert_eq!(bridge.matches("declare global {").count(), 2);

        let contextual = language_service_inline_bridge_for(&[
            state_exposure("user", Some("type"), None, &[""]),
            state_exposure("user", Some("async"), None, &[""]),
        ]);
        assert!(contextual.contains("  const type: any;\n"), "{contextual}");
        assert!(contextual.contains("  const async: any;\n"), "{contextual}");

        let nothing_else = language_service_inline_bridge_for(&[state_exposure(
            "user",
            Some("kind"),
            Some("new"),
            &[""],
        )]);
        assert!(matches!(nothing_else, Cow::Borrowed(_)));
        assert_eq!(nothing_else, language_service_inline_bridge());
    }

    /// The embedded window lib drops the dialog functions and `location`, each with its doc
    /// block, so a state handle may take those names; the vendored file itself still carries
    /// them, and the rest of the lib is untouched.
    #[test]
    fn embedded_window_lib_omits_the_dialogs_and_location() {
        let vendored = DENO_LIB
            .get_file(WINDOW_LIB_FILE)
            .and_then(File::contents_utf8)
            .expect("vendored window lib");
        for name in WINDOW_LIB_OMITTED {
            assert!(
                vendored.contains(&format!("declare function {name}("))
                    || vendored.contains(&format!("declare var {name}:")),
                "the vendored lib declares {name}"
            );
        }
        let window = embedded_language_service_types()
            .into_iter()
            .find(|file| file.virtual_path == WINDOW_LIB_FILE)
            .expect("the snapshot carries the window lib");
        for name in WINDOW_LIB_OMITTED {
            assert!(
                !window
                    .contents
                    .contains(&format!("declare function {name}("))
                    && !window.contents.contains(&format!("declare var {name}:")),
                "{name} must be gone: {}",
                window.contents
            );
        }
        assert!(window.contents.contains("declare var window:"));
        assert!(
            window.contents.contains("declare var Location:"),
            "the Location constructor type stays; only the `location` value goes"
        );
        let kept = vendored.lines().count() - window.contents.lines().count();
        assert!(
            kept > WINDOW_LIB_OMITTED.len(),
            "each dropped declaration takes its doc block with it ({kept} lines dropped)"
        );

        let sample = concat!(
            "/** One.\n",
            " * @category X\n",
            " */\n",
            "declare function alert(message?: string): void;\n",
            "declare var keep: number;\n",
            "/** Two. */\n",
            "declare var location: Location;\n",
            "declare var onunhandledrejection:\n",
            "  | ((this: Window, ev: PromiseRejectionEvent) => any)\n",
            "  | null;\n",
        );
        assert_eq!(
            strip_global_declarations(sample, &["alert", "location", "onunhandledrejection"]),
            concat!(
                "declare var keep: number;\n",
                "declare var onunhandledrejection:\n",
                "  | ((this: Window, ev: PromiseRejectionEvent) => any)\n",
                "  | null;\n",
            ),
            "single-line declarations go with their doc blocks; a multi-line one stays"
        );
    }

    /// A name the libs still declare as a global is left out of the bridge (an ambient
    /// redeclaration would error), while a name the window lib drops is declared.
    #[test]
    fn inline_bridge_declares_dropped_window_names_and_skips_lib_globals() {
        let bridge = language_service_inline_bridge_for(&[
            state_exposure("user", Some("prompt"), None, &[""]),
            state_exposure("user", Some("location"), None, &["room"]),
            state_exposure("user", Some("console"), None, &[""]),
            state_exposure("user", Some("kind"), Some("Math"), &["hp"]),
        ]);
        assert!(bridge.contains("  const prompt: any;\n"), "{bridge}");
        assert!(bridge.contains("  const location: any;\n"), "{bridge}");
        assert!(!bridge.contains("const console:"), "{bridge}");
        assert!(!bridge.contains("const Math:"), "{bridge}");
    }

    /// A handle named `prompt` (the plan's own example) types in the in-app editor: the
    /// window lib no longer declares the dialog, so the bridge's declaration stands, while a
    /// handle named `console` leaves the lib's `console` in place.
    #[test]
    fn in_memory_language_service_types_a_prompt_handle() {
        use service_harness::{inline_diagnostics, install_inline_bridge, open_project};

        let (mut host, client, project) = open_project(91, 92);
        let exposures = [
            state_exposure("user", Some("prompt"), None, &[""]),
            state_exposure("user", Some("console"), None, &[""]),
        ];
        install_inline_bridge(
            &mut host,
            &client,
            project,
            2,
            language_service_inline_bridge_for(&exposures).into_owned(),
        );
        let clean = inline_diagnostics(
            &mut host,
            &client,
            project,
            94,
            "smudgy-inline:///aliases/prompt-handle.ts",
            concat!(
                "const hp: unknown = prompt.hp;\n",
                "console.log(String(hp));\n",
                "send(`hp ${hp}`);\n",
            ),
        );
        assert!(
            clean.is_empty(),
            "a body reading a `prompt` handle must type-check: {clean:?}"
        );
        host.shutdown().expect("the language service shuts down");
    }

    /// An exposing body type-checks against the per-automation bridge: the exposed names
    /// resolve, the platform name carries the declared shape along its exposed paths, and
    /// the `gmcp` it sees is the state value, so the control object's members are errors.
    #[test]
    fn in_memory_language_service_types_exposed_state_names() {
        use smudgy_script::language_service::DiagnosticCode;

        use service_harness::{inline_diagnostics, install_inline_bridge, open_project};

        let (mut host, client, project) = open_project(81, 82);
        let exposures = [
            state_exposure("gmcp", None, None, &["Char.Vitals"]),
            state_exposure("user", Some("foo"), Some("stats"), &["bar"]),
        ];
        install_inline_bridge(
            &mut host,
            &client,
            project,
            2,
            language_service_inline_bridge_for(&exposures).into_owned(),
        );

        let clean = inline_diagnostics(
            &mut host,
            &client,
            project,
            84,
            "smudgy-inline:///aliases/state-values.ts",
            concat!(
                "const hp: number | undefined = gmcp.Char.Vitals?.hp;\n",
                "const maxhp: number | undefined = gmcp.Char.Vitals?.maxhp;\n",
                "const bar: unknown = stats.bar;\n",
                "send(`hp ${hp}/${maxhp} ${bar} ${matches[1]}`);\n",
            ),
        );
        assert!(
            clean.is_empty(),
            "a body reading its exposed names must type-check: {clean:?}"
        );

        let shadowed = inline_diagnostics(
            &mut host,
            &client,
            project,
            85,
            "smudgy-inline:///aliases/state-shadow.ts",
            concat!(
                "gmcp.send(\"Char.Items.Inv\");\n",
                "const wrong: string = gmcp.Char.Vitals?.hp;\n",
                "const room = gmcp.Room;\n",
                "void wrong; void room;\n",
            ),
        );
        let codes = shadowed
            .iter()
            .map(|item| item.code.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            codes.len(),
            3,
            "the control object's member, the mistyped leaf, and the unexposed path: {shadowed:?}"
        );
        assert_eq!(
            codes
                .iter()
                .filter(|code| **code == Some(DiagnosticCode::Number(2339)))
                .count(),
            2,
            "`gmcp.send` and `gmcp.Room` must not exist on the exposed shape: {shadowed:?}"
        );
        assert!(
            codes.contains(&Some(DiagnosticCode::Number(2322))),
            "`gmcp.Char.Vitals.hp` must be typed as the declared number: {shadowed:?}"
        );
        assert!(
            shadowed.iter().any(|item| item.message.contains("'send'")),
            "the shadowed `gmcp` must reject `send`: {shadowed:?}"
        );

        host.shutdown()
            .expect("language-service worker must shut down cleanly");
    }

    #[test]
    fn stale_runtime_typings_are_replaced_without_touching_user_tsconfig() {
        let dir = temp_server_dir("runtime-types-upgrade");
        let managed = dir.join(".smudgy");
        let deno_types = managed.join("types/deno");
        fs::create_dir_all(&deno_types).unwrap();
        fs::write(
            managed.join(".runtime-types-version"),
            "deno-v2.9.0+node-26.0.1+1",
        )
        .unwrap();
        fs::write(deno_types.join("lib.deno.ns.d.ts"), "stale declaration").unwrap();

        let authored = "{ \"compilerOptions\": { \"strict\": false } }";
        fs::write(dir.join("tsconfig.json"), authored).unwrap();

        ensure_script_tsconfig_in(&dir, &[]).expect("upgrade managed runtime typings");

        assert_eq!(
            fs::read_to_string(managed.join(".runtime-types-version"))
                .unwrap()
                .trim(),
            RUNTIME_TYPES_VERSION
        );
        assert_ne!(
            fs::read_to_string(deno_types.join("lib.deno.ns.d.ts")).unwrap(),
            "stale declaration",
            "the stale managed declaration tree must be replaced"
        );
        assert_eq!(
            fs::read_to_string(dir.join("tsconfig.json")).unwrap(),
            authored,
            "the user's own project configuration must remain untouched"
        );

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

        assert!(
            !modules.join(".smudgy").exists(),
            "stale managed dir removed"
        );
        // The stale heavy generated stub is replaced by the thin pointer at the server-level project
        // (it carries the new `../tsconfig.json` extends, not the old base-config marker).
        let modules_ts = fs::read_to_string(modules.join("tsconfig.json"))
            .expect("modules tsconfig seeded after migration");
        assert!(
            modules_ts.contains("../tsconfig.json"),
            "thin pointer expected:\n{modules_ts}"
        );
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
