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
pub mod console;
pub mod host;
pub mod kind;
pub mod manifest;
pub mod plan;
pub mod scan;
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

use std::fmt;
use std::path::Path;

use parsec_orbit_plugin_abi::{InitFn, LegacyInitFn, QueryFn, ShutdownFn};

pub use manifest::{read_manifest, Manifest, KIND_DLL, KIND_JS, MANIFEST};
pub use scan::{scan, Scan, Summary};

/// Severity of a log line, in the order the ABI's `LOG_*` values use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    /// The level behind an ABI `LOG_*` value. Unknown numbers are `Info`,
    /// which is what the loader logged before the values had a type.
    pub fn from_abi(level: u32) -> Level {
        match level {
            0 => Level::Trace,
            1 => Level::Debug,
            3 => Level::Warn,
            4 => Level::Error,
            _ => Level::Info,
        }
    }

    /// The ABI `LOG_*` value of this level.
    pub const fn to_abi(self) -> u32 {
        match self {
            Level::Trace => 0,
            Level::Debug => 1,
            Level::Info => 2,
            Level::Warn => 3,
            Level::Error => 4,
        }
    }

    /// The tag a log line carries.
    pub const fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Sink for the loader's log lines.
pub trait Logger {
    fn log(&self, level: Level, message: &str);

    fn trace(&self, message: &str) {
        self.log(Level::Trace, message);
    }
    fn debug(&self, message: &str) {
        self.log(Level::Debug, message);
    }
    fn info(&self, message: &str) {
        self.log(Level::Info, message);
    }
    fn warn(&self, message: &str) {
        self.log(Level::Warn, message);
    }
    fn error(&self, message: &str) {
        self.log(Level::Error, message);
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
