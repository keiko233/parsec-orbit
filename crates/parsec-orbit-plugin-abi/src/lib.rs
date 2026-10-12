//! C ABI between the Parsec Orbit loader and its plugins.
//!
//! This crate is the single source of truth for the interface between the
//! loader that runs inside the client process and the plugins it maps. Both
//! sides depend on it, so a change here is a change to the ABI - extend
//! [`ParsecOrbitHost`] by appending fields and bump [`ABI_VERSION`] only when a
//! plugin that ignores the new fields would misbehave.
//!
//! The crate is `no_std` on purpose: a plugin does not have to link the Rust
//! standard library to talk to the loader. The C equivalent of these
//! definitions is `docs/spec/plugin-abi.md`; a test keeps the two in step.

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

/// Success.
pub const OK: i32 = 0;
/// Invalid argument.
pub const ERR_INVALID: i32 = -1;
/// Already exists.
pub const ERR_EXISTS: i32 = -2;
/// Not found.
pub const ERR_NOT_FOUND: i32 = -3;
/// Not ready, or not supported by this loader.
pub const ERR_NOT_READY: i32 = -4;
/// Timed out.
pub const ERR_TIMEOUT: i32 = -5;
/// Script or remote exception.
pub const ERR_EXCEPTION: i32 = -6;

/// Prefix of the interface a plugin registers to serve a non-`dll` kind.
pub const INTERFACE_KIND_PREFIX: &str = "parsec-orbit.kind.";
/// Names reserved for the official services.
pub const INTERFACE_WEBVIEW: &str = "parsec-orbit.webview";
pub const INTERFACE_JS: &str = "parsec-orbit.js";
pub const INTERFACE_HOOKS: &str = "parsec-orbit.hooks";
pub const INTERFACE_CONFIG: &str = "parsec-orbit.config";
pub const INTERFACE_MARKET: &str = "parsec-orbit.market";

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

/// Size of [`ParsecOrbitHost`] in revision 1, which ended with `reserved`.
pub const HOST_REVISION_1_SIZE: usize = 96;

/// Services the loader offers to a plugin.
///
/// Each plugin gets a table of its own, so who is calling is data in the table
/// rather than an argument of every callback. Revision 1 ended with
/// `reserved`; revision 2 appends the fields after it, which is why a plugin
/// checks `struct_size` ([`ParsecOrbitHost::has_revision_2`]) before it touches
/// them. Entries are only ever appended.
#[repr(C)]
pub struct ParsecOrbitHost {
    /// ABI version implemented by the loader.
    pub abi_version: u32,
    /// `size_of::<ParsecOrbitHost>()` of the loader's SDK version.
    pub struct_size: u32,
    /// Writes a UTF-8 message to the loader log.
    pub log: Option<unsafe extern "system" fn(level: u32, message: *const u8, len: usize)>,
    /// Looks up a registered interface by NUL-terminated name and returns its
    /// [`ParsecOrbitInterfaceHeader`], or null.
    pub query_interface: Option<unsafe extern "system" fn(name: *const u8) -> *mut c_void>,
    /// Loader version, NUL-terminated UTF-8.
    pub loader_version: *const u8,
    /// Reserved for future host services; stays zero.
    pub reserved: [usize; 8],

    // ---- revision 2 ----
    /// Id of the plugin this table belongs to: the manifest id, or the
    /// descriptor id of a plugin without a manifest. NUL-terminated UTF-8.
    pub plugin_id: *const u8,
    /// Absolute directory of the plugin. NUL-terminated UTF-8.
    pub plugin_dir: *const u8,
    /// `%APPDATA%\ParsecOrbit\data\<id>`; the directory may not exist yet.
    /// Null when the loader has no `%APPDATA%`. NUL-terminated UTF-8.
    pub data_dir: *const u8,
    /// Like `log`, with a target (usually the plugin id) written next to the
    /// message.
    pub log_ex: Option<
        unsafe extern "system" fn(
            level: u32,
            target: *const u8,
            target_len: usize,
            message: *const u8,
            len: usize,
        ),
    >,
    /// Registers an interface table under a NUL-terminated name. `host` is the
    /// caller's own table, which tells the loader who owns the registration.
    /// Returns [`OK`], [`ERR_INVALID`] or [`ERR_EXISTS`].
    pub register_interface: Option<
        unsafe extern "system" fn(
            host: *const ParsecOrbitHost,
            name: *const u8,
            table: *const ParsecOrbitInterfaceHeader,
        ) -> i32,
    >,
    /// Removes an interface the caller registered. Returns [`OK`] or
    /// [`ERR_NOT_FOUND`] (also when somebody else owns the name).
    pub unregister_interface:
        Option<unsafe extern "system" fn(host: *const ParsecOrbitHost, name: *const u8) -> i32>,
    /// Reserved for future host services; stays zero.
    pub reserved2: [usize; 8],
}

/// Reads a NUL-terminated UTF-8 string handed over through the ABI.
///
/// # Safety
/// `pointer` is null or points to a NUL-terminated string that outlives `'a`.
unsafe fn c_str<'a>(pointer: *const u8) -> Option<&'a str> {
    if pointer.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(pointer as *const _) }.to_str().ok()
}

impl ParsecOrbitHost {
    /// A table of the current revision with nothing filled in: the layout
    /// fields are set, every callback is `None`, every string is null. Loaders
    /// and tests start from this and set what they provide.
    pub const fn blank() -> Self {
        ParsecOrbitHost {
            abi_version: ABI_VERSION,
            struct_size: core::mem::size_of::<ParsecOrbitHost>() as u32,
            log: None,
            query_interface: None,
            loader_version: core::ptr::null(),
            reserved: [0; 8],
            plugin_id: core::ptr::null(),
            plugin_dir: core::ptr::null(),
            data_dir: core::ptr::null(),
            log_ex: None,
            register_interface: None,
            unregister_interface: None,
            reserved2: [0; 8],
        }
    }

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
        // Safety: the loader guarantees a NUL-terminated UTF-8 string.
        unsafe { c_str(self.loader_version) }
    }

    /// Whether the loader's table is large enough to hold the revision 2
    /// fields. A revision 1 loader reports [`HOST_REVISION_1_SIZE`] bytes.
    pub fn has_revision_2(&self) -> bool {
        self.struct_size as usize >= core::mem::size_of::<ParsecOrbitHost>()
    }

    /// Id of the plugin this table was made for (revision 2).
    pub fn plugin_id(&self) -> Option<&str> {
        // Safety: the loader keeps the strings alive for the whole process.
        self.has_revision_2()
            .then(|| unsafe { c_str(self.plugin_id) })
            .flatten()
    }

    /// Directory the plugin was loaded from (revision 2).
    pub fn plugin_dir(&self) -> Option<&str> {
        self.has_revision_2()
            .then(|| unsafe { c_str(self.plugin_dir) })
            .flatten()
    }

    /// Directory the plugin may keep its data in (revision 2); it may not
    /// exist yet.
    pub fn data_dir(&self) -> Option<&str> {
        self.has_revision_2()
            .then(|| unsafe { c_str(self.data_dir) })
            .flatten()
    }

    /// Logs with a target, falling back to `log` on a revision 1 loader.
    pub fn log_with_target(&self, level: u32, target: &str, message: &str) {
        if self.has_revision_2() {
            if let Some(log_ex) = self.log_ex {
                // Safety: both slices are valid for the duration of the call.
                unsafe {
                    log_ex(
                        level,
                        target.as_ptr(),
                        target.len(),
                        message.as_ptr(),
                        message.len(),
                    )
                };
                return;
            }
        }
        self.log(level, message);
    }

    /// Looks an interface up by name.
    ///
    /// # Safety
    /// The header is the start of a table whose type `name` documents; the
    /// caller casts it to that type and must honour `header.size`.
    pub unsafe fn interface(&self, name: &CStr) -> Option<&ParsecOrbitInterfaceHeader> {
        let query = self.query_interface?;
        let header =
            unsafe { query(name.as_ptr() as *const u8) } as *const ParsecOrbitInterfaceHeader;
        unsafe { header.as_ref() }
    }

    /// Registers an interface table under `name`. The table has to stay valid
    /// for the rest of the process.
    ///
    /// Returns [`ERR_NOT_READY`] when the loader has no registry (revision 1).
    pub fn register(&self, name: &CStr, table: &'static ParsecOrbitInterfaceHeader) -> i32 {
        if !self.has_revision_2() {
            return ERR_NOT_READY;
        }
        match self.register_interface {
            // Safety: `self` is the table the loader made for this plugin and
            // `table` is `'static`.
            Some(register) => unsafe { register(self, name.as_ptr() as *const u8, table) },
            None => ERR_NOT_READY,
        }
    }
}

/// First member of every interface table, see `docs/spec/services.md`.
///
/// A minor version only appends functions, so a consumer checks that `size`
/// covers a function before it calls it; a different major version is a
/// different contract.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsecOrbitInterfaceHeader {
    /// `size_of` the whole table, header included.
    pub size: u32,
    /// `(major << 16) | minor`.
    pub version: u32,
}

impl ParsecOrbitInterfaceHeader {
    /// Header for a table of `size` bytes at `major.minor`.
    pub const fn new(size: usize, major: u16, minor: u16) -> Self {
        ParsecOrbitInterfaceHeader {
            size: size as u32,
            version: ((major as u32) << 16) | minor as u32,
        }
    }

    pub const fn major(&self) -> u16 {
        (self.version >> 16) as u16
    }

    pub const fn minor(&self) -> u16 {
        (self.version & 0xFFFF) as u16
    }

    /// Whether the table is large enough to hold a member that ends at byte
    /// `end` (`offset_of!(Table, member) + size_of::<member type>()`).
    pub const fn covers(&self, end: usize) -> bool {
        self.size as usize >= end
    }
}

/// What a kind handler is told about the plugin it should load.
#[repr(C)]
pub struct ParsecOrbitPluginLoadInfo {
    /// `size_of::<ParsecOrbitPluginLoadInfo>()`.
    pub size: u32,
    /// Manifest id.
    pub id: *const u8,
    /// Plugin directory, absolute.
    pub dir: *const u8,
    /// Entry file, absolute.
    pub entry: *const u8,
    /// `%APPDATA%\ParsecOrbit\data\<id>`; null when unknown.
    pub data_dir: *const u8,
    /// Raw `plugin.toml` text, UTF-8, not NUL terminated.
    pub manifest: *const u8,
    pub manifest_len: usize,
}

/// `parsec-orbit.kind.<kind>` 1.0: loads plugins of a non-`dll` kind.
#[repr(C)]
pub struct ParsecOrbitKindHandlerV1 {
    pub header: ParsecOrbitInterfaceHeader,
    /// Loads one plugin; `0` is success.
    pub load_plugin:
        Option<unsafe extern "system" fn(info: *const ParsecOrbitPluginLoadInfo) -> i32>,
    /// Unloads a plugin by id; may be null until unloading exists.
    pub unload_plugin: Option<unsafe extern "system" fn(id: *const u8) -> i32>,
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
