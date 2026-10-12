//! C ABI between the Parsec Orbit loader and its plugins.
//!
//! This crate is the single source of truth for the interface between the
//! loader that runs inside the client process and the plugins it maps. Both
//! sides depend on it, so a change here is a change to the ABI - extend
//! [`ParsecOrbitHost`] by appending fields and bump [`ABI_VERSION`] only when a
//! plugin that ignores the new fields would misbehave.
//!
//! The crate is `no_std` on purpose: a plugin does not have to link the Rust
//! standard library to talk to the loader, and the C header equivalent in
//! `docs/spec/plugin-abi.md` is generated from these definitions.

#![no_std]

#[cfg(test)]
extern crate std;

use core::ffi::{c_void, CStr};

/// ABI version implemented by this SDK.
///
/// The loader refuses a plugin whose descriptor asks for a newer version.
pub const ABI_VERSION: u32 = 1;

/// Log levels used by [`ParsecOrbitHost::log`].
pub const LOG_TRACE: u32 = 0;
pub const LOG_DEBUG: u32 = 1;
pub const LOG_INFO: u32 = 2;
pub const LOG_WARN: u32 = 3;
pub const LOG_ERROR: u32 = 4;

/// Exports a plugin has to provide.
pub const EXPORT_QUERY: &str = "ParsecOrbitPluginQuery";
pub const EXPORT_INIT: &str = "ParsecOrbitPluginInit";
pub const EXPORT_SHUTDOWN: &str = "ParsecOrbitPluginShutdown";

/// Export of the original proof of concept, still called when a plugin only
/// provides this one.
pub const EXPORT_LEGACY_INIT: &str = "ParsecPluginInit";

/// Static description of a plugin, returned by [`QueryFn`].
///
/// All strings are NUL-terminated UTF-8 and have to stay valid for the whole
/// lifetime of the module.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ParsecOrbitPluginDesc {
    /// ABI the plugin was built against.
    pub abi_version: u32,
    /// `size_of::<ParsecOrbitPluginDesc>()` of the plugin's SDK version.
    pub struct_size: u32,
    /// Unique id, reverse DNS style, e.g. `com.example.hello`.
    pub id: *const u8,
    /// Human readable name.
    pub name: *const u8,
    /// Plugin version (semver).
    pub version: *const u8,
    /// Author or organisation.
    pub author: *const u8,
    /// One line description.
    pub description: *const u8,
    /// SPDX license identifier of the plugin.
    pub license: *const u8,
    /// Reserved capability bits; the loader logs them as-is for now.
    pub capabilities: u64,
}

/// Services the loader offers to a plugin.
///
/// New entries are appended at the end; `reserved` exists so that a future
/// loader can hand out interfaces without breaking plugins that were built
/// against this layout.
#[repr(C)]
pub struct ParsecOrbitHost {
    /// ABI version implemented by the loader.
    pub abi_version: u32,
    /// `size_of::<ParsecOrbitHost>()` of the loader's SDK version.
    pub struct_size: u32,
    /// Writes a UTF-8 message to the loader log.
    pub log: Option<unsafe extern "system" fn(level: u32, message: *const u8, len: usize)>,
    /// Looks up an optional host interface by name; `None` today, this is the
    /// extension point for hooks, the WebView bridge and the JS runtime.
    pub query_interface: Option<unsafe extern "system" fn(name: *const u8) -> *mut c_void>,
    /// Loader version, NUL-terminated UTF-8.
    pub loader_version: *const u8,
    /// Reserved for future host services.
    pub reserved: [usize; 8],
}

impl ParsecOrbitHost {
    /// Convenience wrapper around the host `log` callback.
    pub fn log(&self, level: u32, message: &str) {
        if let Some(log) = self.log {
            // Safety: `message` is a valid slice for the duration of the call,
            // and the loader only reads it.
            unsafe { log(level, message.as_ptr(), message.len()) }
        }
    }

    /// Loader version as a string, if the loader provided one.
    pub fn loader_version(&self) -> Option<&str> {
        if self.loader_version.is_null() {
            return None;
        }
        // Safety: the loader guarantees a NUL-terminated UTF-8 string.
        unsafe { CStr::from_ptr(self.loader_version as *const _) }
            .to_str()
            .ok()
    }
}

/// `ParsecOrbitPluginQuery(host_abi) -> *const ParsecOrbitPluginDesc`
pub type QueryFn = unsafe extern "system" fn(host_abi: u32) -> *const ParsecOrbitPluginDesc;
/// `ParsecOrbitPluginInit(host) -> 0 on success`
pub type InitFn = unsafe extern "system" fn(host: *const ParsecOrbitHost) -> i32;
/// `ParsecOrbitPluginShutdown() -> 0 on success`
pub type ShutdownFn = unsafe extern "system" fn() -> i32;
/// Legacy single-export entry point.
pub type LegacyInitFn = unsafe extern "system" fn(*mut c_void) -> u32;

/// Wrapper that makes the descriptor usable in a `static`.
///
/// The descriptor itself is not `Sync` because it contains raw pointers; the
/// pointers are never written after the module is loaded, so sharing it is
/// sound.
pub struct SyncDesc(pub ParsecOrbitPluginDesc);

// Safety: the wrapped value is immutable for the lifetime of the process.
unsafe impl Sync for SyncDesc {}

#[cfg(test)]
mod tests;
