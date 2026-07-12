use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

#[macro_use]
extern crate log;

/// Process-wide override for the smudgy home directory. When set, it replaces
/// the default `<Documents>/smudgy` location for every data path in the app.
/// Set once at startup from the `--data-dir` launch flag; see
/// [`set_smudgy_home`].
static SMUDGY_HOME_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

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
/// to `<Documents>/smudgy` for tagged releases **and release candidates**, and
/// `<Documents>/smudgy-dev` for dev/pre-release builds
/// ([`crate::models::settings::is_dev_build`]), so a dev build — which also talks to the
/// dev API — keeps its accounts, servers, and installed packages isolated from the release
/// client's data. A release candidate deliberately shares the release home (and API), so a
/// tester exercises the real release data on the real backend.
///
/// # Errors
///
/// Returns an error if the user's document directory cannot be determined or if the
/// smudgy directory cannot be created.
pub fn get_smudgy_home() -> Result<PathBuf> {
    let dir = if let Some(override_dir) = SMUDGY_HOME_OVERRIDE.get() {
        override_dir.clone()
    } else {
        let mut dir = dirs::document_dir().context("Failed to get user document directory")?;
        dir.push(if crate::models::settings::is_dev_build() {
            "smudgy-dev"
        } else {
            "smudgy"
        });
        dir
    };

    fs::create_dir_all(&dir).context(format!(
        "Failed to create smudgy directory at {}",
        dir.to_string_lossy()
    ))?;

    Ok(dir)
}

/// Initialize logging configuration.
///
/// In debug builds, uses `pretty_env_logger` for colorized console output.
/// In release builds, logs to a file named "smudgy.log" in the smudgy home directory
/// with timestamp information.
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
            std::env::set_var("SMUDGY_LOG", "debug");
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
        build_time::build_time_local!("%Y-%m-%d %H:%M:%S")
    );

    deno_core::JsRuntime::init_platform(None);
    trace!(
        "deno initialized, v8 version {}",
        deno_core::v8::VERSION_STRING
    );
}

pub mod models;
pub mod session;
