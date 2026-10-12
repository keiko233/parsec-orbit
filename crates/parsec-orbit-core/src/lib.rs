//! Plugin loading logic that does not need Win32.
//!
//! The loader proper (`parsec-orbit-loader`) is a `bcrypt.dll` proxy; the
//! decisions about *which* plugins to load, in what shape, and whether a
//! descriptor is acceptable live here, written against two small traits so
//! they can be unit-tested without mapping a single DLL:
//!
//! * [`Platform`] maps a library and resolves the plugin exports,
//! * [`Logger`] receives the one-line decisions the e2e suite asserts on.
//!
//! This crate must stay light (see ADR 0004): no async runtime, no serde.

pub mod activate;
pub mod host;
pub mod kind;
pub mod manifest;
pub mod plan;
pub mod scan;
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

use std::path::Path;

use parsec_orbit_plugin_abi::{
    InitFn, LegacyInitFn, QueryFn, ShutdownFn, LOG_DEBUG, LOG_ERROR, LOG_INFO, LOG_WARN,
};

pub use manifest::{read_manifest, Manifest, KIND_DLL, KIND_JS, MANIFEST};
pub use scan::{scan, Scan, Summary};

/// Sink for the loader's log lines. `level` uses the ABI's `LOG_*` values.
pub trait Logger {
    fn log(&self, level: u32, message: &str);

    fn debug(&self, message: &str) {
        self.log(LOG_DEBUG, message);
    }
    fn info(&self, message: &str) {
        self.log(LOG_INFO, message);
    }
    fn warn(&self, message: &str) {
        self.log(LOG_WARN, message);
    }
    fn error(&self, message: &str) {
        self.log(LOG_ERROR, message);
    }
}

/// Entry points found in a mapped plugin.
#[derive(Clone, Copy, Default)]
pub struct Exports {
    pub query: Option<QueryFn>,
    pub init: Option<InitFn>,
    pub shutdown: Option<ShutdownFn>,
    pub legacy_init: Option<LegacyInitFn>,
}

/// A library the platform mapped, with the plugin exports it resolved.
pub struct Module {
    /// Opaque handle (the `HMODULE` on Windows), kept for a future unload.
    pub handle: usize,
    pub exports: Exports,
}

/// Operating-system services the loading logic needs.
pub trait Platform {
    /// Maps the library at `path` and resolves the plugin exports. The error
    /// text goes into the log as-is.
    fn load(&self, path: &Path) -> Result<Module, String>;
}

/// A mapped plugin. Kept so a future version can unload or reload plugins
/// without restarting the client.
#[derive(Clone)]
pub struct Loaded {
    pub id: String,
    pub module: usize,
    pub shutdown: Option<ShutdownFn>,
}

/// What the loading logic runs against.
#[derive(Clone, Copy)]
pub struct Env<'a> {
    pub platform: &'a dyn Platform,
    pub logger: &'a dyn Logger,
    /// Reported to plugins as `ParsecOrbitHost::loader_version`.
    pub loader_version: &'a str,
    /// Parent of the per-plugin data directories
    /// (`%APPDATA%\ParsecOrbit\data`); `None` when there is no `%APPDATA%`.
    pub data_root: Option<&'a Path>,
}
