#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The `smudgy` executable: a thin shim over the `smudgy_ui` library crate,
//! which holds the entire application (src/lib.rs). Only what must live on
//! the binary crate root stays here — the release-build `windows_subsystem`
//! attribute above is valid only on executables. The library target exists
//! so benches and integration tests can link against the crate's modules.

fn main() -> anyhow::Result<()> {
    smudgy_ui::run()
}
