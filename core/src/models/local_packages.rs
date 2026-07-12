//! Local (authored) `smudgy://` packages: a package you're developing as a folder under
//! `<smudgy_home>/<server>/packages/<name>/` (per-server, beside `modules/`). The folder
//! holds a `smudgy.package.json` manifest plus the module files.
//!
//! While a local package exists, the session's package provider resolves
//! `smudgy://<yourhandle>/<name>` to this folder (an npm-link-style override), so you
//! test it under its real specifier before publishing. Publishing reads the folder and
//! uploads it (create-or-get namespace, then an immutable version). See
//! `smudgy/script/PACKAGES.md`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use smudgy_cloud::{
    highest_satisfying_version, PackageApiClient, PublishDependency, PublishModule,
};
use smudgy_script::PackageManifest;

use crate::get_smudgy_home;

const MANIFEST_FILE: &str = "smudgy.package.json";

/// The owner segment a local package runs under when there is no account nickname
/// (signed out): `smudgy://local/<name>`. Signed in, a local folder overrides the
/// account's own `smudgy://<nickname>/<name>`; signed out there is no nickname, so
/// this reserved placeholder lets local packages still be addressed, enabled, and
/// run (sandboxed to their manifest, like any local override). Reserved on the
/// server so no real account can publish under it and collide.
pub const LOCAL_OWNER: &str = "local";

/// The `smudgy:core` ambient declarations, made available to the publish-time `.d.ts`
/// generator so a package's `import … from "smudgy:core"` resolves while emitting. The
/// same file the editor sees (`script_typings`).
const SMUDGY_CORE_DTS: &str = include_str!("script_typings/smudgy-core.d.ts");

/// The `mapper` ambient declarations (global `Mapper`/`Area`/`Room`/...). Required at
/// publish time because `smudgy-core.d.ts`'s `mapper` member references the global `Mapper`,
/// and so a package using `mapper` resolves identically to the editor.
const SMUDGY_MAPPER_DTS: &str = include_str!("script_typings/smudgy-mapper.d.ts");

/// The `smudgy:widgets` + `smudgy:widgets/jsx-runtime` ambient declarations, so a package's
/// `import … from "smudgy:widgets"` resolves and its `.tsx` modules type-check + emit against
/// the `JSX` namespace at publish time.
const SMUDGY_WIDGETS_DTS: &str = include_str!("script_typings/smudgy-widgets.d.ts");

/// What a successful publish reports back: the published version plus the outcome of the
/// publish-time TypeScript declaration generation. Declaration generation is **best-effort
/// and never fatal** — a package always publishes even if typings can't be produced.
#[derive(Debug, Clone)]
pub struct PublishSummary {
    /// The published version (the manifest's `version`).
    pub version: String,
    /// How many `.d.ts` modules shipped with the version (0 if none were generated).
    pub typings_generated: usize,
    /// Non-fatal warnings from declaration generation (tsc diagnostics, a failed/empty run),
    /// surfaced to the author. Empty on a clean typings pass.
    pub typings_warnings: Vec<String>,
    /// What each `smudgy://` dependency locked to this publish: `(specifier, resolved_version)`.
    /// A publish freezes the whole tree, so the author should be able to see exactly which version
    /// each dependency pinned — a stale range silently pinning an old version is otherwise invisible.
    pub locked_dependencies: Vec<(String, String)>,
    /// Non-fatal warnings about dependency locking — e.g. a declared range that excludes a *newer*
    /// published version (most notably the 0.0.x caret footgun, where `^0.0.1`/`0.0.1` can never
    /// advance past `0.0.1`). Surfaced to the author; never blocks the publish.
    pub dependency_warnings: Vec<String>,
    /// Non-fatal interop-declaration warnings (interop.md §4): duplicate/aliased handle
    /// exports, and handles the previously published version declared that this version
    /// drops — a handle's name is its identity, so a silent rename breaks consumers and
    /// orphans persisted state. Never blocks the publish.
    pub interop_warnings: Vec<String>,
}
const README_FILE: &str = "README.md";
/// The editor-only `tsconfig.json` a copied ("Make a copy") package carries so VS Code types it
/// against the server-level smudgy project. It's scaffolding, never package content, so it is
/// excluded from publishing — treated like a dotfile by [`collect_modules`].
const TSCONFIG_FILE: &str = "tsconfig.json";
/// The body written into a copied package's [`TSCONFIG_FILE`]. From `<server>/packages/<name>/`,
/// `../../tsconfig.json` is the server-level project (`<name>/..` = `packages/`, `packages/..` =
/// `<server>/`), giving the package the shared compiler options + installed-package `paths`.
const PACKAGE_TSCONFIG: &str = "{ \"extends\": \"../../tsconfig.json\" }\n";
/// Directories never published even if present in a package folder — dependency/build cruft
/// that the exclude-list would otherwise recurse into and ship wholesale.
const SKIP_DIRS: [&str; 6] = ["node_modules", "target", "dist", "build", "out", "coverage"];

/// A local package loaded from disk: its manifest, README, and module files.
#[derive(Debug, Clone)]
pub struct LocalPackage {
    pub name: String,
    pub manifest: PackageManifest,
    /// The package's `README.md` (markdown), if present.
    pub readme: Option<String>,
    pub modules: Vec<LocalModule>,
}

/// One module file within a [`LocalPackage`] (`subpath` is relative to the package dir,
/// always forward-slashed).
#[derive(Debug, Clone)]
pub struct LocalModule {
    pub subpath: String,
    /// Raw file bytes — any file in the package dir (text or binary) is publishable.
    pub content: Vec<u8>,
}

fn packages_dir_in(home: &Path, server_name: &str) -> PathBuf {
    home.join(server_name).join("packages")
}

/// `<smudgy_home>/<server>/packages/`.
///
/// # Errors
/// Returns an error if the smudgy home directory cannot be determined.
pub fn packages_dir(server_name: &str) -> Result<PathBuf> {
    Ok(packages_dir_in(&get_smudgy_home()?, server_name))
}

/// Reads one raw file (`subpath` relative to the package dir) from a local package.
///
/// # Errors
/// Returns an error if the smudgy home can't be resolved or the file can't be read.
pub fn read_local_file(server_name: &str, name: &str, subpath: &str) -> Result<String> {
    let path = packages_dir(server_name)?.join(name).join(subpath);
    fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))
}

/// Writes one raw file (`subpath` relative to the package dir) into a local package,
/// creating parent directories as needed.
///
/// # Errors
/// Returns an error if the smudgy home can't be resolved or the file can't be written.
pub fn write_local_file(server_name: &str, name: &str, subpath: &str, content: &str) -> Result<()> {
    let path = packages_dir(server_name)?.join(name).join(subpath);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&path, content).with_context(|| format!("write {}", path.display()))
}

/// Deletes a local package directory and all of its files.
///
/// # Errors
/// Returns an error if the smudgy home can't be resolved or the directory can't be removed.
pub fn delete_local_package(server_name: &str, name: &str) -> Result<()> {
    let dir = packages_dir(server_name)?.join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    }
    Ok(())
}

/// Names of the local packages authored for `server_name`.
///
/// # Errors
/// Returns an error if the smudgy home or packages directory can't be read.
pub fn list_local_packages(server_name: &str) -> Result<Vec<String>> {
    list_local_packages_in(&get_smudgy_home()?, server_name)
}

fn list_local_packages_in(home: &Path, server_name: &str) -> Result<Vec<String>> {
    let dir = packages_dir_in(home, server_name);
    let mut names = Vec::new();
    match fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir())
                    && entry.path().join(MANIFEST_FILE).is_file()
                    && let Some(name) = entry.file_name().to_str()
                {
                    names.push(name.to_string());
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
    }
    names.sort();
    Ok(names)
}

/// Load a local package (`None` if no folder/manifest exists).
///
/// # Errors
/// Returns an error if the manifest is unreadable or invalid, or a module file can't be
/// read.
pub fn load_local_package(server_name: &str, name: &str) -> Result<Option<LocalPackage>> {
    load_local_package_in(&get_smudgy_home()?, server_name, name)
}

fn load_local_package_in(home: &Path, server_name: &str, name: &str) -> Result<Option<LocalPackage>> {
    let dir = packages_dir_in(home, server_name).join(name);
    let manifest_path = dir.join(MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let manifest_text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest = PackageManifest::parse(&manifest_text)
        .map_err(|e| anyhow!("invalid {}: {e}", manifest_path.display()))?;
    let readme = fs::read_to_string(dir.join(README_FILE)).ok();
    let mut modules = Vec::new();
    collect_modules(&dir, &dir, &mut modules)?;
    modules.sort_by(|a, b| a.subpath.cmp(&b.subpath));
    Ok(Some(LocalPackage {
        name: name.to_string(),
        manifest,
        readme,
        modules,
    }))
}

fn collect_modules(root: &Path, dir: &Path, out: &mut Vec<LocalModule>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        // Skip dotfiles AND dot-directories (`.git`, `.cache`, `.env`, …) everywhere, plus
        // well-known dependency/build directories. Without the DIRECTORY guard,
        // the exclude-list (which only names files) would recurse into `.git/`/`node_modules/`
        // and publish their contents — a real way to ship `.git/config` secrets + history.
        // The editor-only `tsconfig.json` is treated like a dotfile too: never published.
        if file_name.starts_with('.')
            || SKIP_DIRS.contains(&file_name.as_ref())
            || file_name == TSCONFIG_FILE
        {
            continue;
        }
        if file_type.is_dir() {
            collect_modules(root, &path, out)?;
        } else if file_type.is_file() {
            // Everything else is a publishable module (any bytes) EXCEPT the manifest (implied)
            // and the README (published separately as `readme`).
            if file_name == MANIFEST_FILE || file_name == README_FILE {
                continue;
            }
            let subpath = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let content = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            out.push(LocalModule { subpath, content });
        }
    }
    Ok(())
}

/// Scaffold a new local package folder with a starter manifest + `index.ts`.
///
/// # Errors
/// Returns an error if the package already exists or the files can't be written.
pub fn scaffold_local_package(server_name: &str, name: &str) -> Result<()> {
    scaffold_local_package_in(&get_smudgy_home()?, server_name, name)
}

fn scaffold_local_package_in(home: &Path, server_name: &str, name: &str) -> Result<()> {
    let dir = packages_dir_in(home, server_name).join(name);
    if dir.exists() {
        bail!("a package named {name} already exists");
    }
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    // The package name is implied by the folder (`name`) — not written into the manifest, so it can
    // never drift from it. `description` is scaffolded empty for the author to fill in.
    let manifest =
        "{\n  \"version\": \"0.1.0\",\n  \"description\": \"\",\n  \"entry\": \"index.ts\"\n}\n";
    fs::write(dir.join(MANIFEST_FILE), manifest)?;
    fs::write(dir.join("index.ts"), "// smudgy package entry\nexport {};\n")?;
    let readme = format!("# {name}\n\nDescribe your package here.\n");
    fs::write(dir.join(README_FILE), readme)?;
    // Same editor project pointer a copied package gets, so a new package opened standalone in
    // VS Code types against the server-level smudgy project. Excluded from publish like a dotfile.
    fs::write(dir.join(TSCONFIG_FILE), PACKAGE_TSCONFIG)
        .with_context(|| format!("write {}", dir.join(TSCONFIG_FILE).display()))?;
    Ok(())
}

/// The media type to publish a module subpath as. Text/code types get a real type; anything
/// unrecognized is `application/octet-stream` so binaries publish faithfully.
fn media_type_for(subpath: &str) -> &'static str {
    let ext = Path::new(subpath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "ts" | "tsx" | "mts" => "application/typescript",
        "js" | "jsx" | "mjs" | "cjs" => "application/javascript",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "css" => "text/css",
        "html" | "htm" => "text/html",
        "wgsl" | "glsl" | "vert" | "frag" | "txt" | "md" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

/// Publish a local package: create-or-get the caller's namespace, then publish an
/// immutable version from the folder. Bump the manifest `version` to ship an update.
///
/// Package names are owner-scoped on the server (the namespace is `(your owner id, name)`), so a
/// fork always publishes under *your* handle and can never clobber another author's package — no
/// client-side rename gate is needed.
///
/// # Errors
/// Returns an error if the package is missing/invalid, or the backend rejects the publish (e.g. a
/// duplicate version → 409).
pub async fn publish_local_package(
    client: &PackageApiClient,
    server_name: &str,
    name: &str,
) -> Result<PublishSummary> {
    let package = load_local_package(server_name, name)?
        .ok_or_else(|| anyhow!("no local package named {name}"))?;
    let entry = package
        .manifest
        .entry
        .clone()
        .unwrap_or_else(|| "index.ts".to_string());

    // The published namespace name is the folder name (`package.name`); the manifest no longer
    // carries a name, so it can never disagree with the folder. The package description comes from
    // the manifest — but `create_package` is create-or-get and won't update an existing namespace's
    // description, so sync it explicitly when the manifest's value has drifted from the server's.
    let view = client
        .create_package(&package.name, &package.manifest.description)
        .await
        .map_err(|e| anyhow!("create package namespace: {e}"))?;
    if view.description != package.manifest.description {
        client
            .patch_package(view.id, Some(&package.manifest.description), None)
            .await
            .map_err(|e| anyhow!("update package description: {e}"))?;
    }

    let authored: Vec<PublishModule> = package
        .modules
        .iter()
        .map(|m| PublishModule {
            subpath: m.subpath.clone(),
            content: m.content.clone(),
            media_type: media_type_for(&m.subpath).to_string(),
            is_entry: m.subpath == entry,
        })
        .collect();
    let authored_count = authored.len();

    // Publish-time TypeScript declarations (best-effort — a package always publishes even
    // if typings fail). The generated `.d.ts` ride as ordinary, non-entry modules.
    let (dts_modules, typings_warnings) = generate_publish_typings(&package.modules).await;
    // The author's own files always win: a generated `.d.ts` is dropped when the package
    // already ships a module at that subpath (a hand-authored declaration). Without this,
    // a package carrying its own `.d.ts` would publish two modules with the same subpath,
    // which the server rejects — cancelling the publish.
    let modules = merge_published_modules(authored, dts_modules);
    let typings_generated = modules.len() - authored_count;

    let manifest_value =
        serde_json::to_value(&package.manifest).context("serialize package manifest")?;

    let (dependencies, dependency_warnings) = lock_dependencies(client, &package.manifest).await?;
    let locked_dependencies = dependencies
        .iter()
        .map(|d| {
            (
                format!("smudgy://{}/{}", d.owner_nickname, d.name),
                d.resolved_version.clone(),
            )
        })
        .collect();

    // Interop-declaration validation + rename diff (interop.md §4) — best-effort and never
    // fatal, like typings.
    let interop_warnings = interop_publish_warnings(client, &package, &entry).await;

    // Ship an optional README.md from the package root (surfaced in discovery + inspect).
    let readme = fs::read_to_string(packages_dir(server_name)?.join(name).join(README_FILE)).ok();

    let published = client
        .publish_version(
            view.id,
            &package.manifest.version,
            &manifest_value,
            &modules,
            &dependencies,
            readme.as_deref(),
        )
        .await
        .map_err(|e| anyhow!("publish version {}: {e}", package.manifest.version))?;

    Ok(PublishSummary {
        version: published.version,
        typings_generated,
        typings_warnings,
        locked_dependencies,
        dependency_warnings,
        interop_warnings,
    })
}

/// Interop-declaration publish validation (interop.md §4). Two sources, both best-effort
/// and never fatal:
/// - the entry's own export-shape problems (duplicate names, aliased exports), promoted
///   from boot-time logs to author-visible publish warnings;
/// - a diff of the declared handle set against the *currently published* version: a handle
///   name is the identity consumers import and persistence keys off, so one that vanishes
///   (usually an innocent const rename under name inference) gets a warning naming the
///   pinning fix. First publish, offline, or a logged-out account skip the diff silently.
async fn interop_publish_warnings(
    client: &PackageApiClient,
    package: &LocalPackage,
    entry: &str,
) -> Vec<String> {
    use smudgy_script::interop_extract::{extract_interop_handles, fold_interop_name};

    let mut warnings = Vec::new();
    let Some(entry_module) = package.modules.iter().find(|m| m.subpath == entry) else {
        return warnings;
    };
    let Ok(url) = deno_core::ModuleSpecifier::parse(&format!("file:///{entry}")) else {
        return warnings;
    };
    let Ok(entry_text) = std::str::from_utf8(&entry_module.content) else {
        return warnings;
    };
    let Ok(extraction) = extract_interop_handles(&url, entry_text) else {
        // A parse error fails the publish elsewhere; not this check's job to report.
        return warnings;
    };
    if !extraction.duplicates.is_empty() {
        warnings.push(format!(
            "duplicate interop handle name(s): {} (first declaration wins)",
            extraction.duplicates.join(", ")
        ));
    }
    warnings.extend(extraction.export_diagnostics.iter().cloned());

    // Rename diff vs the published latest.
    let Some(owner) = crate::models::auth::load_account().and_then(|a| a.nickname) else {
        return warnings;
    };
    let Ok(previous) = client.resolve_package(&owner, &package.name, None).await else {
        return warnings;
    };
    let prev_entry = previous
        .manifest
        .get("entry")
        .and_then(|v| v.as_str())
        .unwrap_or("index.ts")
        .to_string();
    let Some(prev_module) = previous.modules.iter().find(|m| m.subpath == prev_entry) else {
        return warnings;
    };
    let Ok(prev_text) = client
        .fetch_module_body(&prev_module.content_url, &prev_module.content_hash)
        .await
    else {
        return warnings;
    };
    let Ok(prev_url) = deno_core::ModuleSpecifier::parse(&format!("file:///{prev_entry}")) else {
        return warnings;
    };
    let Ok(prev_extraction) = extract_interop_handles(&prev_url, &prev_text) else {
        return warnings;
    };
    let current: std::collections::HashSet<_> = extraction
        .handles
        .iter()
        .map(|h| (h.kind, fold_interop_name(&h.name)))
        .collect();
    for prev in &prev_extraction.handles {
        if !current.contains(&(prev.kind, fold_interop_name(&prev.name))) {
            warnings.push(format!(
                "v{} published the {} handle {:?}, which this version drops — consumers importing it will break. If this is a rename, keep the identity by passing the old name explicitly (e.g. create…({:?}, …))",
                previous.version,
                prev.kind.as_str(),
                prev.name,
                prev.name
            ));
        }
    }
    warnings
}

/// Whether `subpath` names a TypeScript declaration file (`*.d.ts`, `*.d.mts`, `*.d.cts`).
/// A declaration file has a TS-family extension and a stem ending in `.d` (e.g. `index.d.ts`
/// → extension `ts`, stem `index.d`).
fn is_declaration_file(subpath: &str) -> bool {
    let path = Path::new(subpath);
    let is_ts = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ["ts", "mts", "cts"].iter().any(|t| ext.eq_ignore_ascii_case(t)));
    let stem_is_decl = path
        .file_stem()
        .map(Path::new)
        .and_then(|stem| stem.extension())
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("d"));
    is_ts && stem_is_decl
}

/// Whether `subpath` is a TypeScript *source* file the declaration generator should
/// compile (a `.d.ts` is already a declaration). `.tsx` IS compiled — the generator emits
/// via the automatic JSX runtime against the `smudgy:widgets/jsx-runtime` ambient.
fn is_typescript_source(subpath: &str) -> bool {
    let path = Path::new(subpath);
    let is_ts = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ["ts", "mts", "cts", "tsx"].iter().any(|t| ext.eq_ignore_ascii_case(t)));
    is_ts && !is_declaration_file(subpath)
}

/// Combine a package's own modules with the publish-time generated declarations, with the
/// author's files winning on any subpath collision. A generated `.d.ts` is dropped when the
/// package already ships a module at that subpath — a hand-authored declaration the author
/// shipped deliberately — so the published payload never carries two modules with the same
/// subpath (which the server rejects, cancelling the publish). A generated declaration whose
/// subpath the author did not provide is appended as-is.
fn merge_published_modules(
    authored: Vec<PublishModule>,
    generated: Vec<PublishModule>,
) -> Vec<PublishModule> {
    let authored_subpaths: std::collections::HashSet<String> =
        authored.iter().map(|m| m.subpath.clone()).collect();
    let mut merged = authored;
    merged.extend(
        generated
            .into_iter()
            .filter(|m| !authored_subpaths.contains(&m.subpath)),
    );
    merged
}

/// Generate `.d.ts` for a package's TypeScript modules at publish time. **Best-effort and
/// never fatal**: returns the declaration modules to ship plus any warnings (tsc
/// diagnostics, or a generation failure) to surface to the author. The (blocking,
/// isolate-constructing) compiler runs on its own thread via `spawn_blocking`, off both
/// the UI loop and any live session isolates.
async fn generate_publish_typings(modules: &[LocalModule]) -> (Vec<PublishModule>, Vec<String>) {
    let mut sources = BTreeMap::new();
    let mut has_typescript = false;
    for module in modules {
        let is_ts = is_typescript_source(&module.subpath);
        // `.json` data modules join the compile VFS so an
        // `import x from "./data.json" with { type: "json" }` resolves (resolveJsonModule);
        // tsc emits no declarations for the JSON itself — only for the TS that imports it.
        let is_json = module.subpath.ends_with(".json");
        if !is_ts && !is_json {
            continue;
        }
        if let Ok(text) = std::str::from_utf8(&module.content) {
            has_typescript |= is_ts;
            sources.insert(module.subpath.clone(), text.to_string());
        }
    }
    if !has_typescript {
        return (Vec::new(), Vec::new());
    }

    let mut ambient = BTreeMap::new();
    ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
    ambient.insert("smudgy-mapper.d.ts".to_string(), SMUDGY_MAPPER_DTS.to_string());
    ambient.insert("smudgy-widgets.d.ts".to_string(), SMUDGY_WIDGETS_DTS.to_string());

    match tokio::task::spawn_blocking(move || {
        smudgy_script::dts::generate_declarations(&sources, &ambient)
    })
    .await
    {
        Ok(Ok(generated)) => {
            let warnings = generated.diagnostics;
            let dts_modules = generated
                .files
                .into_iter()
                .map(|(subpath, content)| PublishModule {
                    media_type: media_type_for(&subpath).to_string(),
                    is_entry: false,
                    subpath,
                    content: content.into_bytes(),
                })
                .collect();
            (dts_modules, warnings)
        }
        Ok(Err(e)) => (
            Vec::new(),
            vec![format!("declaration generation failed — publishing without typings: {e:#}")],
        ),
        Err(e) => (
            Vec::new(),
            vec![format!("declaration generation panicked — publishing without typings: {e}")],
        ),
    }
}

/// Resolve each declared `smudgy://` dependency range to the concrete highest published
/// version that satisfies it, recording `{specifier, range, resolved_version}`. Installers
/// reproduce this exact dependency set, and the resolution engine dedupes/coexists
/// packages by what each dependent locked. A range with no published match is a publish
/// error naming the dependency.
///
/// Returns the locked dependency set plus any non-fatal **warnings**: a declared range that
/// resolves to an *older* version than the latest published one (so the publish silently freezes
/// a back-level dependency). The most common trap is the 0.0.x caret footgun — under Cargo semver
/// `^0.0.1` and bare `0.0.1` mean `>=0.0.1, <0.0.2`, so they can never advance to `0.0.2`.
async fn lock_dependencies(
    client: &PackageApiClient,
    manifest: &PackageManifest,
) -> Result<(Vec<PublishDependency>, Vec<String>)> {
    let mut dependencies = Vec::new();
    let mut warnings = Vec::new();
    for dep in manifest.smudgy_dependencies() {
        let owner_nickname = dep.key.owner.clone();
        let name = dep.key.name;
        // A range-less dependency means "any version" (`*`); that's also what we record.
        let range = dep.range.unwrap_or_else(|| "*".to_string());

        let resolved = client
            .resolve_package(&owner_nickname, &name, None)
            .await
            .map_err(|e| anyhow!("lock dependency {owner_nickname}/{name}: {e}"))?;
        let versions = client
            .list_versions(resolved.package_id)
            .await
            .map_err(|e| anyhow!("lock dependency {owner_nickname}/{name}: {e}"))?;
        let resolved_version = highest_satisfying_version(&versions, Some(&range))
            .map_err(|e| anyhow!("dependency {owner_nickname}/{name} has an invalid range {range}: {e}"))?
            .ok_or_else(|| {
                anyhow!("no published version of {owner_nickname}/{name} satisfies {range}")
            })?;

        // The highest published version overall is always >= the highest *within* the range (the
        // range is a subset), so if they differ the range is excluding a strictly-newer release.
        // Surface that — it's the silent "bundled an old version" trap — but never block the publish.
        if let Ok(Some(latest)) = highest_satisfying_version(&versions, None)
            && latest != resolved_version
        {
            let hint = if excludes_zero_zero_patch(&range, &resolved_version) {
                " — a caret/bare requirement on a 0.0.x version pins to that exact patch and never \
                 advances; widen it (e.g. \"*\" or \">=0.0.x\")"
            } else {
                " — widen the range or re-publish to pick it up"
            };
            warnings.push(format!(
                "dependency {owner_nickname}/{name}: locked v{resolved_version}, but v{latest} is \
                 published and your range \"{range}\" excludes it{hint}"
            ));
        }

        dependencies.push(PublishDependency {
            owner_nickname,
            name,
            range,
            resolved_version,
        });
    }
    Ok((dependencies, warnings))
}

/// Whether `range` is a caret-or-bare requirement on a `0.0.x` version (which Cargo semver pins to
/// that exact patch, never advancing) and `resolved` is itself a `0.0.x` version — the precise
/// shape of the silent "can't move past 0.0.x" footgun. A tilde (`~0.0.1`) or comparator
/// (`>=0.0.1`) range is *not* flagged: those do admit higher 0.0.x patches.
fn excludes_zero_zero_patch(range: &str, resolved: &str) -> bool {
    let core = range.trim().strip_prefix('^').unwrap_or_else(|| range.trim());
    core.starts_with("0.0.") && resolved.starts_with("0.0.")
}

/// Fork a package's files into a NEW local package at `<server>/packages/<new_name>/`. The fork
/// becomes yours (owner = your handle), keeping the source's files verbatim — the identity is the
/// `new_name` folder (the manifest carries no name). Rejects an existing local package of that
/// name (the local duplicate-name guard).
///
/// `modules` are the source package's files (e.g. a `ResolvedPackage`'s modules mapped to
/// [`LocalModule`], or another local package's). Generated `.d.ts` declaration files are skipped:
/// a published package carries them, but publishing the fork regenerates declarations for the same
/// subpaths, so copying the stale ones would collide on re-publish.
///
/// # Errors
/// Returns an error if a package named `new_name` already exists, or the files can't be
/// written.
pub fn fork_to_local(
    server_name: &str,
    new_name: &str,
    source_manifest: &PackageManifest,
    modules: &[LocalModule],
) -> Result<()> {
    fork_to_local_in(
        &get_smudgy_home()?,
        server_name,
        new_name,
        source_manifest,
        modules,
    )
}

fn fork_to_local_in(
    home: &Path,
    server_name: &str,
    new_name: &str,
    source_manifest: &PackageManifest,
    modules: &[LocalModule],
) -> Result<()> {
    let dir = packages_dir_in(home, server_name).join(new_name);
    if dir.exists() {
        bail!("a package named {new_name} already exists");
    }
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

    // Copied verbatim — the fork's name is its `new_name` folder, not anything in the manifest.
    let manifest_json =
        serde_json::to_string_pretty(source_manifest).context("serialize forked manifest")?;
    fs::write(dir.join(MANIFEST_FILE), manifest_json)?;

    for module in modules {
        // Generated declarations are regenerated at publish time; copying them collides the stale
        // copy with the fresh `.d.ts` of the same subpath on the fork's first re-publish.
        if is_declaration_file(&module.subpath) {
            continue;
        }
        // Subpaths are always forward-slashed + validated; join is in-package.
        let path = dir.join(module.subpath.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        fs::write(&path, &module.content)
            .with_context(|| format!("write {}", path.display()))?;
    }

    // A copied package is its own editor project: drop a thin `tsconfig.json` pointing at the
    // server-level smudgy project so VS Code types it. Written last so it wins over any stale
    // `tsconfig.json` a source might have shipped (it's excluded from publish either way).
    fs::write(dir.join(TSCONFIG_FILE), PACKAGE_TSCONFIG)
        .with_context(|| format!("write {}", dir.join(TSCONFIG_FILE).display()))?;
    Ok(())
}

/// Renames a local package folder (`old` → `new`) under `<server>/packages/`. The manifest
/// carries no name, so the folder name *is* the identity — renaming the folder is the rename.
/// Rejects an empty new name or a target that already exists; a no-op rename succeeds.
///
/// # Errors
/// Returns an error if the smudgy home can't be resolved, `old` doesn't exist, `new` already
/// exists, or the rename fails.
pub fn rename_local_package(server_name: &str, old: &str, new: &str) -> Result<()> {
    rename_local_package_in(&get_smudgy_home()?, server_name, old, new)
}

fn rename_local_package_in(home: &Path, server_name: &str, old: &str, new: &str) -> Result<()> {
    let new = new.trim();
    if new.is_empty() {
        bail!("a package name can't be empty");
    }
    if new == old {
        return Ok(());
    }
    let packages = packages_dir_in(home, server_name);
    let from = packages.join(old);
    let to = packages.join(new);
    if !from.is_dir() {
        bail!("no local package named {old}");
    }
    if to.exists() {
        bail!("a package named {new} already exists");
    }
    fs::rename(&from, &to)
        .with_context(|| format!("rename {} -> {}", from.display(), to.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::VersionListItem;

    #[tokio::test]
    async fn publish_typings_generates_declarations_for_ts_modules() {
        let modules = vec![
            LocalModule {
                subpath: "util.ts".to_string(),
                content: b"export function computeRisk(hp: number) { return hp < 100 ? \"high\" : \"low\"; }\n".to_vec(),
            },
            LocalModule {
                subpath: "index.ts".to_string(),
                content: b"import { computeRisk } from \"./util.ts\";\nexport function describe(hp: number) { return { risk: computeRisk(hp) }; }\n".to_vec(),
            },
            // A non-TS file is ignored by the generator.
            LocalModule {
                subpath: "data.json".to_string(),
                content: b"{}".to_vec(),
            },
        ];

        let (dts, warnings) = generate_publish_typings(&modules).await;
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

        let names: Vec<&str> = dts.iter().map(|m| m.subpath.as_str()).collect();
        assert!(names.contains(&"index.d.ts"), "missing index.d.ts: {names:?}");
        assert!(names.contains(&"util.d.ts"), "missing util.d.ts: {names:?}");
        assert!(dts.iter().all(|m| !m.is_entry), "generated .d.ts must not be entry");

        let index = dts.iter().find(|m| m.subpath == "index.d.ts").unwrap();
        let text = String::from_utf8(index.content.clone()).unwrap();
        assert!(text.contains("describe(hp: number)"), "got:\n{text}");
    }

    #[tokio::test]
    async fn publish_typings_is_empty_when_no_ts_modules() {
        let modules = vec![LocalModule {
            subpath: "readme.txt".to_string(),
            content: b"hello".to_vec(),
        }];
        let (dts, warnings) = generate_publish_typings(&modules).await;
        assert!(dts.is_empty());
        assert!(warnings.is_empty());
    }

    fn module(subpath: &str, content: &[u8]) -> PublishModule {
        PublishModule {
            subpath: subpath.to_string(),
            content: content.to_vec(),
            media_type: media_type_for(subpath).to_string(),
            is_entry: subpath == "index.ts",
        }
    }

    /// A package shipping its own `.d.ts` (e.g. `index.d.ts`) must not collide with the
    /// publish-time generated declaration of the same subpath: the author's file wins and the
    /// generated one is dropped, so the payload carries each subpath exactly once. Previously
    /// this duplicate cancelled the publish (the server rejects duplicate subpaths).
    #[test]
    fn merge_drops_generated_dts_colliding_with_authored_modules() {
        let authored = vec![
            module("index.ts", b"export const x = 1;"),
            module("index.d.ts", b"export declare const x: 1;"),
            module("util.ts", b"export const u = 2;"),
        ];
        let generated = vec![
            module("index.d.ts", b"// generated, must be dropped"),
            module("util.d.ts", b"// generated, kept"),
        ];

        let merged = merge_published_modules(authored, generated);

        let mut subpaths: Vec<&str> = merged.iter().map(|m| m.subpath.as_str()).collect();
        subpaths.sort_unstable();
        assert_eq!(subpaths, ["index.d.ts", "index.ts", "util.d.ts", "util.ts"]);

        // The hand-authored declaration is preserved; the generated one of the same subpath is gone.
        let index_dts = merged.iter().find(|m| m.subpath == "index.d.ts").unwrap();
        assert_eq!(index_dts.content, b"export declare const x: 1;");
        // A generated declaration the author did not provide is appended as-is.
        let util_dts = merged.iter().find(|m| m.subpath == "util.d.ts").unwrap();
        assert_eq!(util_dts.content, b"// generated, kept");
    }

    fn version_item(version: &str, yanked: bool) -> VersionListItem {
        VersionListItem {
            version: version.to_string(),
            yanked,
            deleted: false,
            published_at: "2026-06-20T00:00:00Z".parse().unwrap(),
        }
    }

    /// The range -> concrete dep-lock picker: highest satisfying, yanked excluded, and a
    /// clear no-match for the publish path to surface against the dependency.
    #[test]
    fn dep_lock_picks_highest_satisfying_excluding_yanked() {
        let versions = [
            version_item("1.2.0", false),
            version_item("1.3.0", false),
            version_item("1.4.0", true), // yanked -> excluded
            version_item("2.0.0", false),
        ];
        // `^1.2` collapses to the highest non-yanked `1.x` (1.4.0 is yanked).
        assert_eq!(
            highest_satisfying_version(&versions, Some("^1.2")).unwrap().as_deref(),
            Some("1.3.0")
        );
        // No published `3.x` -> no match (the publish path turns this into a named error).
        assert_eq!(highest_satisfying_version(&versions, Some("^3")).unwrap(), None);
        // No range constraint -> the highest published version overall.
        assert_eq!(
            highest_satisfying_version(&versions, None).unwrap().as_deref(),
            Some("2.0.0")
        );
    }

    /// The 0.0.x caret footgun detector: a caret/bare `0.0.x` range that resolved to a `0.0.x`
    /// version is flagged (it can never advance); tilde/comparator ranges and non-0.0.x versions
    /// are not.
    #[test]
    fn flags_only_the_zero_zero_caret_footgun() {
        assert!(excludes_zero_zero_patch("^0.0.1", "0.0.1"));
        assert!(excludes_zero_zero_patch("0.0.1", "0.0.1")); // bare == caret under Cargo semver
        assert!(excludes_zero_zero_patch(" ^0.0.1 ", "0.0.1")); // whitespace-tolerant
        // Tilde admits higher 0.0.x patches, so it is not the footgun.
        assert!(!excludes_zero_zero_patch("~0.0.1", "0.0.1"));
        // A comparator range admits newer releases.
        assert!(!excludes_zero_zero_patch(">=0.0.1", "0.0.1"));
        // 0.1.x / 1.x carets advance normally — not the 0.0.x trap.
        assert!(!excludes_zero_zero_patch("^0.1.0", "0.1.0"));
        assert!(!excludes_zero_zero_patch("^1.0", "1.0.0"));
    }

    #[test]
    fn scaffold_then_load_round_trips() {
        let home = tempfile::tempdir().unwrap();
        let server = "Arctic";
        assert!(list_local_packages_in(home.path(), server).unwrap().is_empty());

        scaffold_local_package_in(home.path(), server, "mymapper").unwrap();
        assert_eq!(
            list_local_packages_in(home.path(), server).unwrap(),
            vec!["mymapper"]
        );

        let pkg = load_local_package_in(home.path(), server, "mymapper")
            .unwrap()
            .expect("local package loads");
        // Identity is the folder name; the scaffold writes no manifest name and an empty description.
        assert_eq!(pkg.name, "mymapper");
        assert!(pkg.manifest.description.is_empty());
        assert_eq!(pkg.manifest.version, "0.1.0");
        // The editor tsconfig is on disk but not a publishable module — only index.ts is.
        let tsconfig = packages_dir_in(home.path(), server)
            .join("mymapper")
            .join(TSCONFIG_FILE);
        assert_eq!(fs::read_to_string(&tsconfig).unwrap(), PACKAGE_TSCONFIG);
        assert_eq!(pkg.modules.len(), 1);
        assert_eq!(pkg.modules[0].subpath, "index.ts");

        // Re-scaffolding an existing package is rejected.
        assert!(scaffold_local_package_in(home.path(), server, "mymapper").is_err());
    }

    #[test]
    fn loads_nested_modules_with_forward_slash_subpaths() {
        let home = tempfile::tempdir().unwrap();
        let server = "Arctic";
        let dir = packages_dir_in(home.path(), server).join("multi");
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join(MANIFEST_FILE), r#"{ "name": "multi", "version": "1.0.0" }"#).unwrap();
        fs::write(dir.join("index.ts"), "export {};").unwrap();
        fs::write(dir.join("lib").join("util.ts"), "export const u = 1;").unwrap();

        let pkg = load_local_package_in(home.path(), server, "multi").unwrap().unwrap();
        let subpaths: Vec<&str> = pkg.modules.iter().map(|m| m.subpath.as_str()).collect();
        assert!(subpaths.contains(&"index.ts"));
        assert!(subpaths.contains(&"lib/util.ts"));
    }

    #[test]
    fn collect_modules_skips_cruft_dirs_and_dotfiles() {
        let home = tempfile::tempdir().unwrap();
        let server = "Arctic";
        let dir = packages_dir_in(home.path(), server).join("pkg");
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::create_dir_all(dir.join("node_modules").join("dep")).unwrap();
        fs::write(dir.join(MANIFEST_FILE), r#"{ "name": "pkg", "version": "1.0.0" }"#).unwrap();
        fs::write(dir.join(README_FILE), "# pkg").unwrap();
        fs::write(dir.join("index.ts"), "export {};").unwrap();
        fs::write(dir.join(".env"), "SECRET=1").unwrap();
        // The editor-only tsconfig is excluded from publish like a dotfile.
        fs::write(dir.join(TSCONFIG_FILE), PACKAGE_TSCONFIG).unwrap();
        // Cruft that must NEVER be published as modules:
        fs::write(dir.join(".git").join("config"), "[remote] url=https://x:tok@h/r").unwrap();
        fs::write(
            dir.join("node_modules").join("dep").join("index.js"),
            "module.exports={}",
        )
        .unwrap();

        let pkg = load_local_package_in(home.path(), server, "pkg").unwrap().unwrap();
        let subpaths: Vec<&str> = pkg.modules.iter().map(|m| m.subpath.as_str()).collect();
        // Only the real source module is collected — not .git/config (secrets!), not
        // node_modules, not the .env dotfile, not the README/manifest, not the editor tsconfig.
        assert_eq!(subpaths, vec!["index.ts"], "only index.ts is a module; got {subpaths:?}");
    }

    #[test]
    fn missing_package_is_none() {
        let home = tempfile::tempdir().unwrap();
        assert!(load_local_package_in(home.path(), "Arctic", "ghost").unwrap().is_none());
    }

    #[test]
    fn rename_local_package_moves_folder() {
        let home = tempfile::tempdir().unwrap();
        let server = "Arctic";
        let manifest = PackageManifest::parse(r#"{ "version": "1.0.0" }"#).unwrap();
        let modules = vec![LocalModule { subpath: "index.ts".into(), content: "export {};".into() }];
        fork_to_local_in(home.path(), server, "boo", &manifest, &modules).unwrap();

        rename_local_package_in(home.path(), server, "boo", "myboo").unwrap();
        assert!(
            load_local_package_in(home.path(), server, "boo").unwrap().is_none(),
            "the old name is gone after rename"
        );
        let pkg = load_local_package_in(home.path(), server, "myboo").unwrap().unwrap();
        assert_eq!(pkg.name, "myboo");

        // Renaming onto an existing name, or from a missing source, is rejected; a no-op is fine.
        scaffold_local_package_in(home.path(), server, "taken").unwrap();
        assert!(rename_local_package_in(home.path(), server, "myboo", "taken").is_err());
        assert!(rename_local_package_in(home.path(), server, "ghost", "whatever").is_err());
        assert!(rename_local_package_in(home.path(), server, "myboo", "myboo").is_ok());
    }

    #[test]
    fn fork_creates_a_renamed_local_copy() {
        let home = tempfile::tempdir().unwrap();
        let server = "Arctic";
        let manifest =
            PackageManifest::parse(r#"{ "version": "1.2.0", "description": "A mapper", "entry": "index.ts" }"#)
                .unwrap();
        let modules = vec![
            LocalModule {
                subpath: "index.ts".into(),
                content: "export const x = 1;".into(),
            },
            LocalModule {
                subpath: "lib/util.ts".into(),
                content: "export const u = 2;".into(),
            },
            // A published source package ships generated declarations; the fork must drop them.
            LocalModule {
                subpath: "index.d.ts".into(),
                content: "export declare const x: number;".into(),
            },
            LocalModule {
                subpath: "lib/util.d.ts".into(),
                content: "export declare const u: number;".into(),
            },
        ];

        fork_to_local_in(home.path(), server, "mymapper", &manifest, &modules).unwrap();

        // A copied package carries the thin editor tsconfig pointing two levels up at the server
        // project — on disk, but excluded from the published module set (treated like a dotfile).
        let tsconfig = packages_dir_in(home.path(), server)
            .join("mymapper")
            .join(TSCONFIG_FILE);
        assert_eq!(fs::read_to_string(&tsconfig).unwrap(), PACKAGE_TSCONFIG);
        assert!(PACKAGE_TSCONFIG.contains("../../tsconfig.json"));

        // The fork loads as a local package with the NEW (folder) name + the copied manifest/modules.
        let pkg = load_local_package_in(home.path(), server, "mymapper").unwrap().unwrap();
        assert_eq!(pkg.name, "mymapper");
        assert_eq!(pkg.manifest.description, "A mapper");
        assert_eq!(pkg.manifest.version, "1.2.0");
        let subpaths: Vec<&str> = pkg.modules.iter().map(|m| m.subpath.as_str()).collect();
        assert!(subpaths.contains(&"index.ts"));
        assert!(subpaths.contains(&"lib/util.ts"));
        // The tsconfig is not a publishable module.
        assert!(!subpaths.contains(&TSCONFIG_FILE), "tsconfig must be excluded; got {subpaths:?}");
        // Generated declarations are not copied — they'd collide with publish-time regeneration.
        assert!(
            !subpaths.iter().any(|s| s.ends_with(".d.ts")),
            "generated .d.ts must not be copied into the fork; got {subpaths:?}"
        );
        let dir = packages_dir_in(home.path(), server).join("mymapper");
        assert!(!dir.join("index.d.ts").exists(), "index.d.ts must not be written to disk");
        assert!(!dir.join("lib").join("util.d.ts").exists(), "lib/util.d.ts must not be written to disk");

        // Forking over an existing name is rejected.
        assert!(fork_to_local_in(home.path(), server, "mymapper", &manifest, &modules).is_err());
    }
}
