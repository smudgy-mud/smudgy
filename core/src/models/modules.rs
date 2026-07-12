use crate::get_smudgy_home;
use anyhow::{Context, Result};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Represents a discovered module file within a server's `modules` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleFile {
    /// The module's path relative to the server's `modules/` directory,
    /// forward-slashed (e.g. "`auto_healer.ts`" or "`combat/healer.ts`").
    pub subpath: String,
    /// The full path to the module file.
    pub path: PathBuf,
}

/// Lists the module files found within a server's `modules` directory, recursing into
/// subdirectories. Each entry's `subpath` is forward-slashed and relative to the
/// `modules/` root, sorted lexically (parents before children). The smudgy-generated
/// `tsconfig.json` editor pointer is excluded — it's a VS Code artifact, not a module.
///
/// # Arguments
///
/// * `server_name` - The name of the server whose modules should be listed.
///
/// # Errors
///
/// Returns an error if the server or modules directory cannot be accessed.
/// If the `modules` directory doesn't exist, an empty list is returned successfully.
pub fn list_modules(server_name: &str) -> Result<Vec<ModuleFile>> {
    let modules_dir = get_smudgy_home()?.join(server_name).join("modules");
    let mut module_files = Vec::new();
    collect_module_files(&modules_dir, &modules_dir, &mut module_files)
        .with_context(|| format!("Failed to read modules for server '{server_name}'"))?;
    module_files.sort_by(|a, b| a.subpath.cmp(&b.subpath));
    Ok(module_files)
}

fn collect_module_files(root: &Path, dir: &Path, out: &mut Vec<ModuleFile>) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        // A missing `modules` directory is not an error — just no modules.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_module_files(root, &path, out)?;
        } else if file_type.is_file() {
            // Skip the smudgy-generated `modules/tsconfig.json` — a thin VS Code project pointer
            // (see `script_typings`) that lives alongside real modules but isn't one. Excluding it
            // here keeps it out of both the sidebar list and the module count.
            if path.file_name().and_then(|n| n.to_str()) == Some("tsconfig.json") {
                continue;
            }
            let subpath = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push(ModuleFile { subpath, path });
        }
    }

    Ok(())
}
