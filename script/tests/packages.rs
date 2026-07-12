//! End-to-end tests for the `smudgy://` shared-package scheme against an in-memory
//! [`PackageProvider`]. The headline is `same_package_imported_twice_is_one_instance`
//! — the shared-isolate guarantee that "installing" a package and a script importing
//! it resolve to the *same* module instance (see `DESIGN.md`).

use std::path::Path;
use std::rc::Rc;

use anyhow::Result;
use deno_core::{serde_v8, ModuleSpecifier, PollEventLoopOptions};
use smudgy_script::{
    InMemoryPackageProvider, ModulePolicy, ModuleSet, PackageKey, PackageManifest,
    PackageModuleSource, ResolvedPackage, ScriptRuntime, ScriptRuntimeOptions,
};

fn tokio_runtime() -> Rc<tokio::runtime::Runtime> {
    Rc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    )
}

fn key(name: &str) -> PackageKey {
    PackageKey {
        owner: "wbk".into(),
        name: name.into(),
    }
}

fn resolved(name: &str, version: &str, modules: &[(&str, &str)]) -> ResolvedPackage {
    ResolvedPackage {
        key: key(name),
        resolved_version: version.into(),
        manifest: PackageManifest::parse(&format!(
            r#"{{ "name": "{name}", "version": "{version}" }}"#
        ))
        .unwrap(),
        integrity: format!("sha256-{name}-{version}"),
        modules: modules
            .iter()
            .map(|(subpath, text)| PackageModuleSource {
                subpath: (*subpath).to_string(),
                text: (*text).to_string(),
            })
            .collect(),
    }
}

fn runtime_with_packages(
    data_dir: &Path,
    packages: Vec<ResolvedPackage>,
) -> Result<(Rc<tokio::runtime::Runtime>, ScriptRuntime)> {
    let tokio = tokio_runtime();
    let mut provider = InMemoryPackageProvider::new();
    for package in packages {
        provider.insert(package);
    }
    let runtime = ScriptRuntime::new(ScriptRuntimeOptions {
        extensions: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        webstorage_dir: None,
        module_policy: ModulePolicy { allow_https: true, ..Default::default() },
        inspector: None,
        tokio: tokio.clone(),
        package_provider: Some(Rc::new(provider)),
        permissions: None,
    })?;
    Ok((tokio, runtime))
}

/// Load a `file://` module that imports a package and exposes a boolean `ok` export.
fn eval_module_ok(
    tokio: &tokio::runtime::Runtime,
    rt: &mut ScriptRuntime,
    data_dir: &Path,
    file_name: &str,
    source: &str,
) -> Result<bool> {
    let path = data_dir.join(file_name);
    std::fs::write(&path, source)?;
    let specifier = ModuleSpecifier::from_file_path(&path).unwrap();
    tokio.block_on(async {
        // A side module: `load_modules` already registered the one allowed "main"
        // module (the synthetic install entry), so the verifier loads as a side module.
        let module_id = rt.deno_runtime().load_side_es_module(&specifier).await?;
        let receiver = rt.deno_runtime().mod_evaluate(module_id);
        rt.deno_runtime()
            .run_event_loop(PollEventLoopOptions::default())
            .await?;
        receiver.await?;

        let namespace = rt.deno_runtime().get_module_namespace(module_id)?;
        deno_core::scope!(scope, rt.deno_runtime());
        let namespace = namespace.open(scope);
        let ok_key = deno_core::v8::String::new(scope, "ok").unwrap();
        let value = namespace.get(scope, ok_key.into()).unwrap();
        Ok(serde_v8::from_v8(scope, value)?)
    })
}

#[test]
fn package_loads_and_evaluates() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let package = resolved(
        "counter",
        "1.0.0",
        &[(
            "index.js",
            "globalThis.__loaded = (globalThis.__loaded ?? 0) + 1;\nexport const x = 1;",
        )],
    );
    let (tokio, mut rt) = runtime_with_packages(temp.path(), vec![package])?;

    let set = ModuleSet {
        local_modules: Vec::new(),
        packages: vec!["smudgy://wbk/counter".to_string()],
    };
    let report = tokio.block_on(async { rt.load_modules(&set).await })?;
    assert_eq!(report.modules.len(), 1);
    assert_eq!(report.modules[0].specifier, "smudgy://wbk/counter");

    // The package evaluated exactly once.
    let ok = eval_module_ok(
        &tokio,
        &mut rt,
        temp.path(),
        "verify_loaded.js",
        "export const ok = globalThis.__loaded === 1;",
    )?;
    assert!(ok, "package should have evaluated exactly once on install");
    Ok(())
}

#[test]
fn package_json_module_imports_with_attribute() -> Result<()> {
    // A package's `.json` module is importable with `with { type: "json" }`: the transpiler
    // preserves the attribute, the loader serves ModuleType::Json for the nested subpath, and
    // the default export is the parsed value. (Without the attribute deno_core rejects a Json
    // module, matching stock Deno.) This is the arctic-newbie-maps shape: per-city data files.
    let temp = tempfile::tempdir()?;
    let package = resolved(
        "mappack",
        "1.0.0",
        &[
            (
                "index.ts",
                "import data from \"./maps/kalaman.json\" with { type: \"json\" };\n\
                 globalThis.__json_map = data;\n",
            ),
            (
                "maps/kalaman.json",
                r#"{ "name": "Kalaman (Newbie)", "rooms": [1, 2, 3] }"#,
            ),
        ],
    );
    let (tokio, mut rt) = runtime_with_packages(temp.path(), vec![package])?;

    let set = ModuleSet {
        local_modules: Vec::new(),
        packages: vec!["smudgy://wbk/mappack".to_string()],
    };
    tokio.block_on(async { rt.load_modules(&set).await })?;

    let ok = eval_module_ok(
        &tokio,
        &mut rt,
        temp.path(),
        "verify_json.js",
        "export const ok = globalThis.__json_map.name === \"Kalaman (Newbie)\"\n\
             && globalThis.__json_map.rooms.length === 3;",
    )?;
    assert!(ok, "the JSON module's parsed value must round-trip through the package import");
    Ok(())
}

#[test]
fn same_package_imported_twice_is_one_instance() -> Result<()> {
    // The shared-isolate guarantee: "installing" the package (via the ModuleSet) and a
    // later script `import` resolve to the SAME instance, so module-level state runs
    // once. A counter that increments per evaluation proves it: if the second import
    // re-evaluated, the count would be 2.
    let temp = tempfile::tempdir()?;
    let package = resolved(
        "counter",
        "1.0.0",
        &[(
            "index.js",
            "globalThis.__evals = (globalThis.__evals ?? 0) + 1;\nexport const evals = globalThis.__evals;\nexport const stamp = { n: globalThis.__evals };",
        )],
    );
    let (tokio, mut rt) = runtime_with_packages(temp.path(), vec![package])?;

    // Install (auto-load on session start).
    let set = ModuleSet {
        local_modules: Vec::new(),
        packages: vec!["smudgy://wbk/counter".to_string()],
    };
    tokio.block_on(async { rt.load_modules(&set).await })?;

    // A user script imports the same package: same resolved canonical URL → one
    // instance → no re-evaluation, and the exported object is identical.
    let ok = eval_module_ok(
        &tokio,
        &mut rt,
        temp.path(),
        "verify_instance.js",
        r#"
        import { evals, stamp } from "smudgy://wbk/counter";
        // Import a second time under the same specifier; must be the same module object.
        import * as again from "smudgy://wbk/counter";
        export const ok = evals === 1
          && globalThis.__evals === 1
          && stamp === again.stamp;
        "#,
    )?;
    assert!(
        ok,
        "install + import must resolve to one shared instance (counter == 1)"
    );
    Ok(())
}

#[test]
fn package_relative_submodule_resolves() -> Result<()> {
    // A package whose entry imports a sibling module via a relative path: the relative
    // import must join against the canonical URL and stay within the package@version.
    let temp = tempfile::tempdir()?;
    let package = resolved(
        "withsub",
        "1.2.0",
        &[
            (
                "index.js",
                "export { u } from \"./lib/util.js\";\nexport const ok = true;",
            ),
            ("lib/util.js", "export const u = 42;"),
        ],
    );
    let (tokio, mut rt) = runtime_with_packages(temp.path(), vec![package])?;

    let ok = eval_module_ok(
        &tokio,
        &mut rt,
        temp.path(),
        "verify_sub.js",
        r#"
        import { u } from "smudgy://wbk/withsub";
        export const ok = u === 42;
        "#,
    )?;
    assert!(ok, "relative submodule import within a package must resolve");
    Ok(())
}

#[test]
fn package_subpath_import_resolves() -> Result<()> {
    // Addressing a module within a multi-module package by subpath.
    let temp = tempfile::tempdir()?;
    let package = resolved(
        "withsub",
        "1.2.0",
        &[
            ("index.ts", "export const ok = true;"),
            ("lib/util.ts", "export const u = 7;"),
        ],
    );
    let (tokio, mut rt) = runtime_with_packages(temp.path(), vec![package])?;

    let ok = eval_module_ok(
        &tokio,
        &mut rt,
        temp.path(),
        "verify_subpath.js",
        r#"
        import { u } from "smudgy://wbk/withsub/lib/util";
        export const ok = u === 7;
        "#,
    )?;
    assert!(ok, "subpath import must resolve to the named module");
    Ok(())
}

#[test]
fn typescript_package_transpiles() -> Result<()> {
    // Package modules are transpiled like local ones (the canonical URL carries the
    // real extension, so the .ts module is transpiled).
    let temp = tempfile::tempdir()?;
    let package = resolved(
        "tspkg",
        "0.1.0",
        &[(
            "index.ts",
            "const value: number = 41;\nexport const answer: number = value + 1;",
        )],
    );
    let (tokio, mut rt) = runtime_with_packages(temp.path(), vec![package])?;

    let ok = eval_module_ok(
        &tokio,
        &mut rt,
        temp.path(),
        "verify_ts.js",
        r#"
        import { answer } from "smudgy://wbk/tspkg";
        export const ok = answer === 42;
        "#,
    )?;
    assert!(ok, "a TypeScript package module must transpile and load");
    Ok(())
}

#[test]
fn load_report_surfaces_manifest_metadata() -> Result<()> {
    // The LoadReport carries each package's resolved version + declared params/hosts so
    // the host can prompt for required params at install time (see `DESIGN.md`). The manifest
    // here uses the "options" key, an accepted alias for "params", to exercise that alias.
    let temp = tempfile::tempdir()?;
    let mut package = resolved("configured", "2.1.0", &[("index.js", "export const x = 1;")]);
    package.manifest = PackageManifest::parse(
        r#"{
            "name": "configured",
            "version": "2.1.0",
            "hosts": ["mud.arctic.org"],
            "options": [
                { "key": "pg.url", "label": "Postgres URL", "secret": true, "required": true }
            ],
            "permissions": { "net": ["comms.coreclan.org:6379"] }
        }"#,
    )?;
    let (tokio, mut rt) = runtime_with_packages(temp.path(), vec![package])?;

    let set = ModuleSet {
        local_modules: Vec::new(),
        packages: vec!["smudgy://wbk/configured".to_string()],
    };
    let report = tokio.block_on(async { rt.load_modules(&set).await })?;

    let info = report.modules[0].package.as_ref().expect("package metadata");
    assert_eq!(info.resolved_version, "2.1.0");
    assert_eq!(info.integrity, "sha256-configured-2.1.0");
    assert_eq!(info.hosts, vec!["mud.arctic.org"]);
    assert_eq!(info.params.len(), 1);
    assert!(info.params[0].secret && info.params[0].required);
    assert_eq!(info.permissions.net, vec!["comms.coreclan.org:6379"]);
    Ok(())
}

#[test]
fn smudgy_params_is_scoped_to_the_importing_package() -> Result<()> {
    // Each package imports `smudgy:params`; the synthesized module binds `get` to THAT
    // package's specifier (from the canonical referrer), so two packages reading the same
    // key get their own namespace. A stub `globalThis.__smudgy_param_get` (the host hook
    // the smudgy ops extension installs in production) echoes `spec|key` so we can prove
    // which package's namespace each call resolved to.
    let temp = tempfile::tempdir()?;
    let app = resolved(
        "app",
        "1.0.0",
        &[("index.js", "import { get } from \"smudgy:params\";\nexport const readUrl = () => get(\"pg.url\");")],
    );
    let other = resolved(
        "other",
        "1.0.0",
        &[("index.js", "import { get } from \"smudgy:params\";\nexport const readUrl = () => get(\"pg.url\");")],
    );
    let (tokio, mut rt) = runtime_with_packages(temp.path(), vec![app, other])?;

    let set = ModuleSet {
        local_modules: Vec::new(),
        packages: vec![
            "smudgy://wbk/app".to_string(),
            "smudgy://wbk/other".to_string(),
        ],
    };
    tokio.block_on(async { rt.load_modules(&set).await })?;

    let ok = eval_module_ok(
        &tokio,
        &mut rt,
        temp.path(),
        "verify_params.js",
        r#"
        // Install the host hook (the real one bridges to op_smudgy_param_get); the import
        // bodies above only DEFINE get, so the hook just needs to exist before we call it.
        globalThis.__smudgy_param_get = (spec, key) => `${spec}|${key}`;
        import { readUrl as appUrl } from "smudgy://wbk/app";
        import { readUrl as otherUrl } from "smudgy://wbk/other";
        export const ok =
            appUrl() === "smudgy://wbk/app|pg.url" &&
            otherUrl() === "smudgy://wbk/other|pg.url";
        "#,
    )?;
    assert!(ok, "each package's smudgy:params.get must resolve to its own namespace");
    Ok(())
}

#[test]
fn missing_package_fails_load() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (tokio, mut rt) = runtime_with_packages(temp.path(), Vec::new())?;
    let set = ModuleSet {
        local_modules: Vec::new(),
        packages: vec!["smudgy://wbk/absent".to_string()],
    };
    let result = tokio.block_on(async { rt.load_modules(&set).await });
    assert!(result.is_err(), "an unresolvable package must fail the load");
    Ok(())
}
