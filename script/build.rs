//! Embeds the vendored TypeScript `lib.*.d.ts` files into the binary as a
//! `&[(name, contents)]` table, so the publish-time declaration generator
//! (`src/dts.rs`) can serve them to an in-memory CompilerHost — no filesystem at runtime.
//!
//! The compiler itself (`typescript.js`) is embedded directly via `include_str!` in
//! `dts.rs`; only the variable-length set of lib files is generated here.

use std::{env, fs, path::PathBuf};

fn main() {
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

    let out_path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("dts_libs.rs");
    fs::write(&out_path, generated)
        .unwrap_or_else(|e| panic!("write {}: {e}", out_path.display()));

    println!("cargo:rerun-if-changed={}", lib_dir.display());
    println!(
        "cargo:rerun-if-changed={}",
        PathBuf::from(&manifest_dir)
            .join("vendor/typescript/lib/typescript.js")
            .display()
    );
}
