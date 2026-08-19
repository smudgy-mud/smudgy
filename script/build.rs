//! Two build-time jobs, both writing into `OUT_DIR` for `src/` to include:
//!
//! 1. **TypeScript lib table** (`dts_libs.rs`): embeds the vendored TypeScript
//!    `lib.*.d.ts` files as a `&[(name, contents)]` table, so the publish-time
//!    declaration generator (`src/dts.rs`) can serve them to an in-memory
//!    CompilerHost — no filesystem at runtime. The compiler itself
//!    (`typescript.js`) is embedded directly via `include_str!` in `dts.rs`;
//!    only the variable-length set of lib files is generated here.
//!
//! 2. **V8 startup snapshot** (`SMUDGY_RUNTIME_SNAPSHOT.bin` +
//!    `residual_lazy_sources.rs`): deno_core 0.410 is snapshot-first — every
//!    `include_js_files!` source (and deno_runtime's `99_main.js`) is recorded
//!    as an absolute build-machine path, never embedded. A runtime without a
//!    startup snapshot reads those paths at every `JsRuntime::new`, which fails
//!    on any machine but the builder. The snapshot bakes the deno_runtime BASE
//!    extension set only; smudgy's own extensions (smudgy_ops / smudgy_mapper /
//!    smudgy_widgets) carry per-session state, live in crates above this one,
//!    and instead embed their ESM via each `extension!`'s customizer.
//!    `residual_lazy_sources.rs` holds `(specifier, source)` tables for
//!    `lazy_loaded_*` files the snapshot did not consume
//!    (`WorkerOptions::residual_lazy_{js,esm}_sources`); they load on demand
//!    via `core.loadExtScript()` / lazy ESM imports, so they must ship embedded.
//!
//!    `create_runtime_snapshot` emits `cargo:rerun-if-changed` for every source
//!    it reads; the generated `include_str!`s are tracked by cargo itself.
//!
//!    The `SnapshotOptions` baked here only feed the snapshot-time bootstrap;
//!    at runtime deno_runtime re-inserts `SnapshotOptions::default()` whenever
//!    a startup snapshot is present, so what scripts observe is computed on the
//!    running machine either way.

use std::collections::HashSet;
use std::{env, fs, path::PathBuf};

use deno_core::{ModuleCodeString, ModuleName};
use deno_runtime::ops::bootstrap::SnapshotOptions;
use deno_runtime::snapshot::{LazyExtensionFileKind, create_runtime_snapshot};

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    write_dts_libs(&out_dir);
    write_startup_snapshot(&out_dir);
}

fn write_dts_libs(out_dir: &PathBuf) {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let lib_dir = PathBuf::from(&manifest_dir).join("vendor/typescript/lib");

    let mut names: Vec<String> = fs::read_dir(&lib_dir)
        .unwrap_or_else(|e| panic!("read vendored tsc lib dir {}: {e}", lib_dir.display()))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("lib") && name.ends_with(".d.ts"))
        .collect();
    names.sort();

    let mut generated = String::from("pub static LIBS: &[(&str, &str)] = &[\n");
    for name in &names {
        let path = lib_dir.join(name);
        // Absolute path so the generated `include_str!` (which lives in OUT_DIR) resolves.
        generated.push_str(&format!(
            "    ({:?}, include_str!({:?})),\n",
            name,
            path.to_string_lossy()
        ));
        println!("cargo:rerun-if-changed={}", path.display());
    }
    generated.push_str("];\n");

    let out_path = out_dir.join("dts_libs.rs");
    fs::write(&out_path, generated).unwrap_or_else(|e| panic!("write {}: {e}", out_path.display()));

    println!("cargo:rerun-if-changed={}", lib_dir.display());
    println!(
        "cargo:rerun-if-changed={}",
        PathBuf::from(&manifest_dir)
            .join("vendor/typescript/lib/typescript.js")
            .display()
    );
}

fn write_startup_snapshot(out_dir: &PathBuf) {
    let snapshot_path = out_dir.join("SMUDGY_RUNTIME_SNAPSHOT.bin");
    let output = create_runtime_snapshot(snapshot_path, SnapshotOptions::default(), Vec::new());

    let consumed: HashSet<&str> = output
        .consumed_lazy_specifiers
        .iter()
        .map(String::as_str)
        .collect();

    let mut js_rows = String::new();
    let mut esm_rows = String::new();
    for file in &output.lazy_extension_files {
        if consumed.contains(file.specifier.as_str()) {
            continue;
        }
        // The runtime registers residual sources VERBATIM (no transpiler runs
        // on the FromSnapshot lazy-load path), so TypeScript residuals (the
        // deno_node polyfills etc.) must be type-stripped here — the same
        // transpile the snapshot build applies to consumed sources.
        let raw = fs::read_to_string(&file.path)
            .unwrap_or_else(|e| panic!("read residual lazy source {}: {e}", file.path.display()));
        let (code, _source_map) = deno_runtime::transpile::maybe_transpile_source(
            ModuleName::from(file.specifier.clone()),
            ModuleCodeString::from(raw),
        )
        .unwrap_or_else(|e| panic!("transpile residual lazy source {}: {e}", file.specifier));
        println!("cargo:rerun-if-changed={}", file.path.display());

        let row = format!("    ({:?}, {:?}),\n", file.specifier, code.as_str());
        match file.kind {
            LazyExtensionFileKind::Js => js_rows.push_str(&row),
            LazyExtensionFileKind::Esm => esm_rows.push_str(&row),
        }
    }

    let out_path = out_dir.join("residual_lazy_sources.rs");
    fs::write(
        &out_path,
        format!(
            "// Generated by build.rs (see there) — lazy_loaded_* sources not baked\n\
             // into the startup snapshot, embedded for on-demand load at runtime.\n\
             pub(crate) static RESIDUAL_LAZY_JS_SOURCES: &[(&str, &str)] = &[\n{js_rows}];\n\
             pub(crate) static RESIDUAL_LAZY_ESM_SOURCES: &[(&str, &str)] = &[\n{esm_rows}];\n"
        ),
    )
    .unwrap_or_else(|e| panic!("write {}: {e}", out_path.display()));
}
