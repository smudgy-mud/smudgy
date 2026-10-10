//! Editor-only view of the npm graph already downloaded by the runtime.
//!
//! TypeScript needs a node_modules graph for declaration dependencies. Copies keep
//! editor writes away from executable cache files; directory links preserve exact
//! dependency versions and cycles without recursively duplicating the graph.

use anyhow::{Context, Result, ensure};
use deno_fs::{FileSystem, FsFileType, RealFs};
use deno_permissions::CheckedPath;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Receives an imported npm specifier and its declaration (or JavaScript) path
/// in the managed editor graph, after the graph is complete.
pub type NpmTypeObserver = Arc<dyn Fn(&str, &Path) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct NpmEditor {
    pub directory: PathBuf,
    pub observer: NpmTypeObserver,
}

#[derive(Serialize)]
pub(crate) struct EditorPackage {
    pub id: String,
    pub directory: PathBuf,
    pub dependencies: BTreeMap<String, String>,
}

// Editor IO is optional work. Bound concurrent copies without occupying the
// runtime's blocking pool with threads waiting on a filesystem mutex.
static EDITOR_WORK_LIMIT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

impl NpmEditor {
    pub fn enqueue(self, packages: Vec<EditorPackage>, target: PathBuf, specifier: String) {
        tokio::spawn(async move {
            let Ok(_permit) = EDITOR_WORK_LIMIT.acquire().await else {
                return;
            };
            let label = specifier.clone();
            let result = tokio::task::spawn_blocking(move || -> Result<()> {
                let projected = materialize(&self.directory, &packages, &target)?;
                (self.observer)(&specifier, &projected);
                Ok(())
            })
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    log::warn!("Could not refresh npm editor types for {label}: {error:#}")
                }
                Err(error) => log::warn!("Npm editor task failed for {label}: {error}"),
            }
        });
    }
}

pub(crate) fn materialize(
    directory: &Path,
    packages: &[EditorPackage],
    target: &Path,
) -> Result<PathBuf> {
    let mut ordered: Vec<_> = packages.iter().collect();
    ordered.sort_by(|left, right| left.id.cmp(&right.id));
    // Include the selected dependency graph, not just package versions: peer and
    // transitive resolutions can differ between isolates and later imports.
    let graph = format!("{:x}", Sha256::digest(serde_json::to_vec(&ordered)?));
    let root = directory.join("v1").join(&graph[..32]);
    let folders: BTreeMap<_, _> = ordered
        .iter()
        .enumerate()
        .map(|(index, package)| (package.id.as_str(), format!("p{index}")))
        .collect();
    let target = fs::canonicalize(target)?;
    let (package, subpath) = ordered
        .iter()
        .find_map(|package| {
            let source = fs::canonicalize(&package.directory).ok()?;
            let relative = target.strip_prefix(source).ok()?.to_path_buf();
            Some((*package, relative))
        })
        .context("npm type target lies outside the resolved package graph")?;

    if !root.join(".ready").is_file() {
        fs::create_dir_all(root.parent().unwrap())?;
        let temporary = tempfile::Builder::new()
            .prefix(".npm-types-")
            .tempdir_in(root.parent().unwrap())?;
        for package in &ordered {
            copy_package(
                &package.directory,
                &temporary.path().join(&folders[package.id.as_str()]),
            )?;
        }
        // Links point to the final locations; no reader sees the graph until all
        // files and dependency links have been created and the directory is renamed.
        for package in &ordered {
            for (name, dependency) in &package.dependencies {
                let Some(folder) = folders.get(dependency.as_str()) else {
                    continue;
                };
                ensure!(valid_dependency_name(name), "invalid npm dependency name");
                let link = temporary
                    .path()
                    .join(&folders[package.id.as_str()])
                    .join("node_modules")
                    .join(name);
                fs::create_dir_all(link.parent().unwrap())?;
                let destination = root.join(folder);
                RealFs
                    .symlink_sync(
                        &CheckedPath::unsafe_new(destination.as_path().into()),
                        &CheckedPath::unsafe_new(link.as_path().into()),
                        Some(if cfg!(windows) {
                            FsFileType::Junction
                        } else {
                            FsFileType::Directory
                        }),
                    )
                    .context("link npm editor dependency")?;
            }
        }
        fs::write(temporary.path().join(".ready"), b"1")?;
        if let Err(error) = fs::rename(temporary.path(), &root) {
            // Another process may have completed the same immutable graph.
            if !root.join(".ready").is_file() {
                return Err(error.into());
            }
        }
    }
    Ok(root.join(&folders[package.id.as_str()]).join(subpath))
}

fn valid_dependency_name(name: &str) -> bool {
    let segments: Vec<_> = name.split('/').collect();
    let shape = if name.starts_with('@') {
        segments.len() == 2
    } else {
        segments.len() == 1
    };
    shape
        && segments.iter().all(|part| {
            !part.is_empty() && *part != "." && *part != ".." && !part.contains(['\\', ':', '\0'])
        })
}

fn copy_package(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == "node_modules" {
            continue;
        }
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_package(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), target)?;
        }
        // Never follow archive symlinks into files outside the downloaded package.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(root: &Path, id: &str, dependencies: &[(&str, &str)]) -> EditorPackage {
        let directory = root.join(id);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("index.d.ts"),
            format!("export type Value = '{id}';"),
        )
        .unwrap();
        EditorPackage {
            id: id.to_string(),
            directory,
            dependencies: dependencies
                .iter()
                .map(|(name, id)| (name.to_string(), id.to_string()))
                .collect(),
        }
    }

    #[test]
    fn editor_graph_preserves_versions_cycles_and_executable_cache() {
        let temporary = tempfile::tempdir().unwrap();
        let cache = temporary.path().join("cache");
        let mut packages = vec![
            package(&cache, "app", &[("left", "left"), ("right", "right")]),
            package(&cache, "left", &[("@scope/shared", "shared-1")]),
            package(&cache, "right", &[("@scope/shared", "shared-2")]),
            package(&cache, "shared-1", &[("app", "app")]),
            package(&cache, "shared-2", &[]),
        ];
        let target = cache.join("app/index.d.ts");
        let directory = temporary.path().join("editor");
        let entry = materialize(&directory, &packages, &target).unwrap();
        let app = entry.parent().unwrap();
        let left = fs::canonicalize(app.join("node_modules/left")).unwrap();
        let right = fs::canonicalize(app.join("node_modules/right")).unwrap();
        let first = fs::canonicalize(left.join("node_modules/@scope/shared")).unwrap();
        let second = fs::canonicalize(right.join("node_modules/@scope/shared")).unwrap();
        assert!(
            fs::read_to_string(first.join("index.d.ts"))
                .unwrap()
                .contains("shared-1")
        );
        assert!(
            fs::read_to_string(second.join("index.d.ts"))
                .unwrap()
                .contains("shared-2")
        );
        assert_eq!(
            fs::canonicalize(first.join("node_modules/app")).unwrap(),
            fs::canonicalize(app).unwrap()
        );
        packages.reverse();
        assert_eq!(materialize(&directory, &packages, &target).unwrap(), entry);
        fs::write(&entry, "editor-only change").unwrap();
        assert!(fs::read_to_string(target).unwrap().contains("export type"));
    }

    #[test]
    fn editor_graph_rejects_dependency_paths_outside_node_modules() {
        let temporary = tempfile::tempdir().unwrap();
        let cache = temporary.path().join("cache");
        let packages = [package(&cache, "app", &[("../../escape", "app")])];
        let result = materialize(
            &temporary.path().join("editor"),
            &packages,
            &cache.join("app/index.d.ts"),
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid npm dependency name")
        );
        assert!(!temporary.path().join("escape").exists());
    }
}
