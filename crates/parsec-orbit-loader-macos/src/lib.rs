//! The macOS Orbit loader: the dylib the patched `parsecd` loads as a
//! dependency.
//!
//! On Windows the loader is a `bcrypt.dll` proxy sitting next to the client;
//! macOS has no equivalent load-order slot, so the installer adds a
//! `LC_LOAD_DYLIB` command to the official bootstrap and dyld maps this
//! library before the stub's code runs. The bootstrap, the official client
//! dylib and the stub's hash validation stay untouched
//! (`docs/spec/macos-host.md`).
//!
//! This crate carries the platform half of the loader: [`MacPlatform`] maps
//! plugin libraries with `dlopen`/`dlsym` and [`log`] appends the loader log
//! lines. Which plugins load, in what order and with which environment is
//! decided by `parsec-orbit-core`, exactly as on Windows.
//!
//! The crate only exists for macOS hosts; on other targets it compiles to an
//! empty library (the CI Windows job still builds the whole workspace).
#![cfg(target_os = "macos")]

mod activation;
mod bootstrap;
mod client;
mod config;
mod isolation;
pub mod log;
pub mod log_observer;
#[allow(dead_code)]
mod nss;
mod original;
mod platform;
pub mod readiness;
mod redirect;
mod runtime;
mod seed;

#[cfg(feature = "harness")]
mod harness;

pub use config::Config;
pub use platform::MacPlatform;
pub use seed::{seed, SeedReport};

use std::sync::{Mutex, OnceLock};

use parsec_orbit_core::{Env, Loaded, Logger, Summary};

/// Version reported to plugins through the host table.
pub const LOADER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Sends the core's lines to the log file.
struct Sink;

impl Logger for Sink {
    fn log(&self, level: parsec_orbit_core::Level, message: &str) {
        log::log(level, message);
    }
}

static SINK: Sink = Sink;

/// Every module a scan mapped, for the life of the process.
///
/// There is no unload (`docs/spec/macos-platform.md`, section 4): interface
/// tables point into the mapped images, so the bookkeeping is kept instead of
/// dropped when the scan result goes away.
static OWNED: OnceLock<Mutex<Vec<Loaded>>> = OnceLock::new();

fn remember(loaded: Vec<Loaded>) {
    let owned = OWNED.get_or_init(|| Mutex::new(Vec::new()));
    match owned.lock() {
        Ok(mut owned) => owned.extend(loaded),
        // A poisoned lock cannot be fixed from here; keeping the modules
        // mapped matters more than the bookkeeping.
        Err(_) => std::mem::forget(loaded),
    }
}

/// Runs one scan with the given settings.
///
/// The logger is installed once per process (the first call wins), matching
/// the Windows loader: host callbacks keep pointing at the same sink.
pub fn scan(config: &Config) -> Summary {
    parsec_orbit_core::host::install_logger(&SINK);
    let result = parsec_orbit_core::scan(
        Env {
            platform: &MacPlatform,
            logger: &SINK,
            loader_version: LOADER_VERSION,
            data_root: config.data_root.as_deref(),
        },
        &config.plugin_dirs,
    );
    remember(result.loaded);
    log::info(&format!(
        "ready: {} loaded, {} skipped, {} failed",
        result.summary.loaded, result.summary.skipped, result.summary.failed
    ));
    result.summary
}
