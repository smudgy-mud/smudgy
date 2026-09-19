use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[macro_use]
extern crate log;

pub mod storage;

/// UTC timestamp embedded when this crate was compiled.
pub const BUILD_TIME_UTC: &str = build_time::build_time_utc!("%Y-%m-%d %H:%M:%S UTC");

/// Git commit embedded by `build.rs`, when the source is in a Git checkout or
/// the build environment supplies a recognized commit variable.
pub const BUILD_COMMIT: Option<&str> = option_env!("SMUDGY_GIT_COMMIT");

/// Process-wide override for the smudgy home directory. When set, it replaces
/// the default `<Documents>/smudgy` location for every data path in the app.
/// Set once at startup from the `--data-dir` launch flag; see
/// [`set_smudgy_home`].
static SMUDGY_HOME_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();
static SMUDGY_HOME_OVERRIDE_PREPARED: OnceLock<()> = OnceLock::new();
static DEFAULT_SMUDGY_HOME: OnceLock<PathBuf> = OnceLock::new();
static DEFAULT_SMUDGY_HOME_FALLBACK_WARNING: OnceLock<String> = OnceLock::new();

struct ResolvedDefaultHome {
    path: PathBuf,
    fallback_warning: Option<String>,
}

fn smudgy_dir_name() -> &'static str {
    if crate::models::settings::is_dev_build() {
        "smudgy-dev"
    } else {
        "smudgy"
    }
}

fn ensure_writable_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("Failed to create smudgy directory at {}", path.display()))?;

    // `create_dir_all` succeeds when the directory already exists even if the
    // application cannot create anything inside it. Probe a real child entry so
    // Windows Controlled Folder Access and equivalent permission failures are
    // detected before this location is selected for the process.
    let probe_dir = tempfile::Builder::new()
        .prefix(".smudgy-write-probe-")
        .tempdir_in(path)
        .with_context(|| format!("Failed to write in smudgy directory at {}", path.display()))?;
    tempfile::Builder::new()
        .prefix("probe-")
        .tempfile_in(probe_dir.path())
        .with_context(|| format!("Failed to write in smudgy directory at {}", path.display()))?
        .close()
        .with_context(|| format!("Failed to remove write probe file from {}", path.display()))?;
    probe_dir.close().with_context(|| {
        format!(
            "Failed to remove write probe directory from smudgy directory at {}",
            path.display()
        )
    })?;

    Ok(())
}

fn resolve_default_smudgy_home_with(
    document_dir: Option<PathBuf>,
    local_data_dir: Option<PathBuf>,
    mut prepare: impl FnMut(&Path) -> Result<()>,
) -> Result<ResolvedDefaultHome> {
    let documents_home = document_dir.map(|path| path.join(smudgy_dir_name()));
    let local_home = local_data_dir.map(|path| path.join(smudgy_dir_name()));

    // Once fallback data exists, keep using it until the user deliberately
    // moves it. Otherwise, a later Defender reputation change could make the
    // empty Documents location writable and make existing servers appear to
    // vanish after a restart. Real Documents data always retains precedence.
    let documents_has_servers = documents_home
        .as_deref()
        .is_some_and(crate::models::server::contains_valid_server);
    let local_home_already_exists = local_home.as_deref().is_some_and(Path::is_dir);

    let mut documents_failure = None;
    if documents_has_servers {
        let documents_home = documents_home
            .as_deref()
            .expect("valid Documents servers require a Documents directory");
        match prepare(documents_home) {
            Ok(()) => {
                return Ok(ResolvedDefaultHome {
                    path: documents_home.to_path_buf(),
                    fallback_warning: None,
                });
            }
            Err(error) => documents_failure = Some(error),
        }
    } else if local_home_already_exists {
        let local_home = local_home
            .as_deref()
            .expect("existing local home requires a local data directory");
        if prepare(local_home).is_ok() {
            let fallback_warning = documents_home.as_deref().map(|documents_home| {
                format!(
                    "The preferred Documents location at {} contains no valid servers; continuing \
                     to use the existing local data directory at {}",
                    documents_home.display(),
                    local_home.display()
                )
            });
            return Ok(ResolvedDefaultHome {
                path: local_home.to_path_buf(),
                fallback_warning,
            });
        }
    }

    if !documents_has_servers && let Some(documents_home) = documents_home.as_deref() {
        match prepare(documents_home) {
            Ok(()) => {
                return Ok(ResolvedDefaultHome {
                    path: documents_home.to_path_buf(),
                    fallback_warning: None,
                });
            }
            Err(error) => documents_failure = Some(error),
        }
    }

    let local_home =
        local_home.context("Failed to get the user Documents or local data directory")?;

    prepare(&local_home).with_context(|| {
        documents_home
            .as_ref()
            .zip(documents_failure.as_ref())
            .map_or_else(
                || {
                    format!(
                        "Failed to prepare fallback smudgy directory at {}",
                        local_home.display()
                    )
                },
                |(documents_home, documents_error)| {
                    format!(
                        "Failed to prepare fallback smudgy directory at {} after the Documents \
                         location at {} failed: {documents_error:#}",
                        local_home.display(),
                        documents_home.display()
                    )
                },
            )
    })?;

    let fallback_warning = documents_home
        .zip(documents_failure)
        .map(|(documents_home, error)| {
            format!(
                "Smudgy could not write to its preferred Documents location at {} ({error:#}); \
                 using {} instead",
                documents_home.display(),
                local_home.display()
            )
        });

    Ok(ResolvedDefaultHome {
        path: local_home,
        fallback_warning,
    })
}

/// Points the smudgy home directory at `path` instead of `<Documents>/smudgy`
/// for the lifetime of the process, isolating this instance's accounts, maps,
/// settings, logs, and session token from the default install.
///
/// This is the backing for the `--data-dir` launch flag, which lets a second
/// instance run side by side against a different account. It **must** be called
/// before any data access — in particular before [`init`], which opens the log
/// file under the home directory. A second call is ignored (the first override
/// wins).
pub fn set_smudgy_home(path: impl Into<PathBuf>) {
    if SMUDGY_HOME_OVERRIDE.set(path.into()).is_err() {
        warn!("smudgy home directory override already set; ignoring later override");
    }
}

/// Returns the path to the smudgy home directory, creating it if it doesn't exist.
///
/// Honors the [`set_smudgy_home`] override when one was set at startup; otherwise defaults
/// to `<Documents>/smudgy` for releases and prod-like prereleases, and
/// `<Documents>/smudgy-dev` for dev/pre-release builds. If that location is not writable (for
/// example, when Windows Controlled Folder Access blocks Smudgy), the platform-local data
/// directory is used instead. The local data directory is also used on systems without a
/// discoverable Documents directory (for example, a minimal/headless Linux account).
/// ([`crate::models::settings::is_dev_build`]), so a dev build — which also talks to the
/// dev API — keeps its accounts, servers, and installed packages isolated from the release
/// client's data. RC, PTB, and nightly builds deliberately share the release
/// home (and API), so a tester exercises real release data on the real backend.
///
/// # Errors
///
/// Returns an error if neither the user's document nor data directory can be determined, or
/// if the smudgy directory cannot be created.
pub fn get_smudgy_home() -> Result<PathBuf> {
    if let Some(override_dir) = SMUDGY_HOME_OVERRIDE.get() {
        if SMUDGY_HOME_OVERRIDE_PREPARED.get().is_none() {
            ensure_writable_directory(override_dir)?;
            let _ = SMUDGY_HOME_OVERRIDE_PREPARED.set(());
        }
        return Ok(override_dir.clone());
    }

    if let Some(dir) = DEFAULT_SMUDGY_HOME.get() {
        return Ok(dir.clone());
    }

    let resolved = resolve_default_smudgy_home_with(
        dirs::document_dir(),
        dirs::data_local_dir(),
        ensure_writable_directory,
    )?;

    // Resolution can race during startup. The first successful choice becomes
    // process-wide so data never moves between roots while Smudgy is running.
    if DEFAULT_SMUDGY_HOME.set(resolved.path.clone()).is_ok()
        && let Some(warning) = resolved.fallback_warning
    {
        eprintln!("Warning: {warning}");
        let _ = DEFAULT_SMUDGY_HOME_FALLBACK_WARNING.set(warning);
    }

    DEFAULT_SMUDGY_HOME
        .get()
        .cloned()
        .context("Failed to cache the resolved smudgy home directory")
}

/// Initialize logging configuration.
///
/// In debug builds, uses `pretty_env_logger` for colorized console output.
/// In release builds, logs to a file named "smudgy.log" in the smudgy home directory
/// with timestamp information.
///
/// Default level (when `SMUDGY_LOG` is unset): `debug` in debug builds,
/// `info` in release builds — a production smudgy.log carries operational
/// events, never the debug stream. Set `SMUDGY_LOG` explicitly to override
/// either default.
///
/// # Errors
///
/// Returns an error if logging initialization fails or if the log file cannot be created
/// in release builds.
fn init_logging() -> Result<()> {
    // Set default log level if not specified
    if std::env::var("SMUDGY_LOG").is_err() {
        // This only needs to be wrapped with unsafe because it isn't thread-safe;
        // this is ok because we're only going to use this once, on the current thread
        unsafe {
            std::env::set_var(
                "SMUDGY_LOG",
                if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "info"
                },
            );
        }
    }

    #[cfg(debug_assertions)]
    {
        // Debug build: use pretty console logger
        pretty_env_logger::try_init_timed_custom_env("SMUDGY_LOG")
            .context("Failed to initialize pretty logger")?;
    }

    #[cfg(not(debug_assertions))]
    {
        // Release build: use file logger
        use simplelog::*;
        use std::fs::File;

        let log_level = match std::env::var("SMUDGY_LOG")
            .unwrap_or_else(|_| "warn".to_string())
            .to_lowercase()
            .as_str()
        {
            "trace" => LevelFilter::Trace,
            "debug" => LevelFilter::Debug,
            "info" => LevelFilter::Info,
            "warn" => LevelFilter::Warn,
            "error" => LevelFilter::Error,
            _ => LevelFilter::Debug,
        };

        let smudgy_home =
            get_smudgy_home().context("Failed to get smudgy home directory for logging")?;
        let log_file_path = smudgy_home.join("smudgy.log");

        let log_file = File::create(&log_file_path).context(format!(
            "Failed to create log file at {}",
            log_file_path.display()
        ))?;

        WriteLogger::init(log_level, Config::default(), log_file)
            .context("Failed to initialize file logger")?;

        if let Some(warning) = DEFAULT_SMUDGY_HOME_FALLBACK_WARNING.get() {
            warn!("{warning}");
        }
    }

    Ok(())
}

pub fn init() {
    // Initialize logging
    if let Err(e) = init_logging() {
        eprintln!("Failed to initialize logging: {e}");
        // Continue execution even if logging fails
    }

    // Route panics through the logger: in windowed release builds stderr is
    // invisible, so without this a panic leaves no trace in smudgy.log.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let thread = std::thread::current();
        error!(
            "thread '{}' {}\nstack backtrace:\n{}",
            thread.name().unwrap_or("<unnamed>"),
            panic_info,
            std::backtrace::Backtrace::force_capture()
        );
        log::logger().flush();
        default_hook(panic_info);
    }));

    info!(
        "smudgy started; version {} ({}, built on {})",
        env!("SMUDGY_BUILD_NAME"),
        env!("CARGO_PKG_VERSION"),
        BUILD_TIME_UTC
    );

    deno_core::JsRuntime::init_platform(None);
    trace!(
        "deno initialized, v8 version {}",
        deno_core::v8::VERSION_STRING
    );
}

pub(crate) mod json;
pub mod models;
pub mod session;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smudgy_base_prefers_writable_documents() {
        let documents = PathBuf::from("documents");
        let local_data = PathBuf::from("local-data");
        let mut attempted = Vec::new();

        let resolved =
            resolve_default_smudgy_home_with(Some(documents.clone()), Some(local_data), |path| {
                attempted.push(path.to_path_buf());
                Ok(())
            })
            .unwrap();

        assert_eq!(resolved.path, documents.join(smudgy_dir_name()));
        assert!(resolved.fallback_warning.is_none());
        assert_eq!(attempted, [documents.join(smudgy_dir_name())]);
    }

    #[test]
    fn smudgy_base_falls_back_when_documents_is_not_writable() {
        let documents = PathBuf::from("documents");
        let local_data = PathBuf::from("local-data");
        let documents_home = documents.join(smudgy_dir_name());
        let local_home = local_data.join(smudgy_dir_name());
        let mut attempted = Vec::new();

        let resolved =
            resolve_default_smudgy_home_with(Some(documents), Some(local_data), |path| {
                attempted.push(path.to_path_buf());
                if path == documents_home {
                    anyhow::bail!("simulated access denial");
                }
                Ok(())
            })
            .unwrap();

        assert_eq!(resolved.path, local_home);
        assert!(
            resolved
                .fallback_warning
                .as_deref()
                .is_some_and(|warning| warning.contains("simulated access denial"))
        );
        assert_eq!(attempted, [documents_home, local_home]);
    }

    #[test]
    fn smudgy_base_keeps_an_existing_fallback_when_documents_has_no_servers() {
        let root = tempfile::tempdir().unwrap();
        let documents = root.path().join("documents");
        let local_data = root.path().join("local-data");
        let documents_home = documents.join(smudgy_dir_name());
        let local_home = local_data.join(smudgy_dir_name());
        fs::create_dir_all(&local_home).unwrap();
        let mut attempted = Vec::new();

        let resolved =
            resolve_default_smudgy_home_with(Some(documents), Some(local_data), |path| {
                attempted.push(path.to_path_buf());
                Ok(())
            })
            .unwrap();

        assert_eq!(resolved.path, local_home);
        assert!(resolved.fallback_warning.is_some());
        assert_eq!(attempted, [local_home]);
        assert!(!documents_home.exists());
    }

    #[test]
    fn smudgy_base_prefers_documents_servers_over_an_existing_fallback() {
        let root = tempfile::tempdir().unwrap();
        let documents = root.path().join("documents");
        let local_data = root.path().join("local-data");
        let documents_home = documents.join(smudgy_dir_name());
        let local_home = local_data.join(smudgy_dir_name());
        let server_home = documents_home.join("example");
        fs::create_dir_all(&server_home).unwrap();
        fs::create_dir_all(&local_home).unwrap();
        fs::write(
            server_home.join("server.json"),
            serde_json::to_vec(&crate::models::server::ServerConfig::new(
                "example.com".to_string(),
                23,
            ))
            .unwrap(),
        )
        .unwrap();
        let mut attempted = Vec::new();

        let resolved =
            resolve_default_smudgy_home_with(Some(documents), Some(local_data), |path| {
                attempted.push(path.to_path_buf());
                Ok(())
            })
            .unwrap();

        assert_eq!(resolved.path, documents_home);
        assert!(resolved.fallback_warning.is_none());
        assert_eq!(attempted, [documents_home]);
    }

    #[test]
    fn smudgy_base_uses_local_data_without_documents() {
        let local_data = PathBuf::from("local-data");

        let resolved =
            resolve_default_smudgy_home_with(None, Some(local_data.clone()), |_| Ok(())).unwrap();

        assert_eq!(resolved.path, local_data.join(smudgy_dir_name()));
        assert!(resolved.fallback_warning.is_none());
        assert!(resolve_default_smudgy_home_with(None, None, |_| Ok(())).is_err());
    }

    #[test]
    fn writable_directory_probe_leaves_no_file_behind() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("smudgy");

        ensure_writable_directory(&home).unwrap();

        assert!(home.is_dir());
        assert_eq!(fs::read_dir(home).unwrap().count(), 0);
    }
}
