//! The loader side of host revision 2: the interface registry, the shared
//! host callbacks and the per-plugin host tables.
//!
//! The callbacks in [`ParsecOrbitHost`] are plain `extern "system"` functions,
//! so the state they work on is process wide: one registry and one log sink.
//! Both are reachable only through this module.

use std::ffi::{c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use parsec_orbit_plugin_abi::{
    ParsecOrbitHost, ParsecOrbitInterfaceHeader, ERR_EXISTS, ERR_INVALID, ERR_NOT_FOUND,
    INTERFACE_KIND_PREFIX, LOG_DEBUG, LOG_INFO, OK,
};

use crate::Logger;

/// Prefix of the ids of plugins that ship with the loader. Only they may
/// register names under [`RESERVED_PREFIX`].
pub const OFFICIAL_ID_PREFIX: &str = "com.parsec-orbit.";

/// Names under this prefix belong to official services. Kind handlers
/// (`parsec-orbit.kind.<kind>`) are open to every plugin that implements a
/// kind.
pub const RESERVED_PREFIX: &str = "parsec-orbit.";

const MAX_NAME_LEN: usize = 128;

// ---- registry -------------------------------------------------------------

struct Entry {
    name: String,
    owner: String,
    table: usize,
}

/// Interface tables by name. Tables are immutable and live for the rest of
/// the process, so the registry only stores their addresses.
#[derive(Default)]
pub struct Registry {
    entries: Vec<Entry>,
}

/// Whether `name` is an acceptable interface name: 1 to 128 ASCII letters,
/// digits, `.`, `-` or `_`, with no empty dot-separated part.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && name.split('.').all(|part| !part.is_empty())
}

impl Registry {
    pub const fn new() -> Self {
        Registry {
            entries: Vec::new(),
        }
    }

    /// Registers `table` under `name` on behalf of `owner`.
    ///
    /// Returns [`OK`], [`ERR_INVALID`] (bad name, null or misaligned table,
    /// header smaller than itself, reserved name of a third party) or
    /// [`ERR_EXISTS`].
    pub fn register(
        &mut self,
        owner: &str,
        name: &str,
        table: *const ParsecOrbitInterfaceHeader,
    ) -> i32 {
        if !valid_name(name)
            || table.is_null()
            || (table as usize) % std::mem::align_of::<ParsecOrbitInterfaceHeader>() != 0
        {
            return ERR_INVALID;
        }
        // Safety: non-null, aligned, and the caller promises a table that
        // starts with a header.
        let size = unsafe { (*table).size } as usize;
        if size < std::mem::size_of::<ParsecOrbitInterfaceHeader>() {
            return ERR_INVALID;
        }
        if name.starts_with(RESERVED_PREFIX)
            && !name.starts_with(INTERFACE_KIND_PREFIX)
            && !owner.starts_with(OFFICIAL_ID_PREFIX)
        {
            return ERR_INVALID;
        }
        if self.entries.iter().any(|entry| entry.name == name) {
            return ERR_EXISTS;
        }
        self.entries.push(Entry {
            name: name.to_string(),
            owner: owner.to_string(),
            table: table as usize,
        });
        OK
    }

    /// The table registered under `name`, or null.
    pub fn find(&self, name: &str) -> *const ParsecOrbitInterfaceHeader {
        self.entries
            .iter()
            .find(|entry| entry.name == name)
            .map_or(std::ptr::null(), |entry| {
                entry.table as *const ParsecOrbitInterfaceHeader
            })
    }

    /// Who registered `name`.
    pub fn owner(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.owner.as_str())
    }

    /// Removes `name` if `owner` registered it. Returns [`OK`] or
    /// [`ERR_NOT_FOUND`], which is also what somebody else's name gets.
    pub fn unregister(&mut self, owner: &str, name: &str) -> i32 {
        match self
            .entries
            .iter()
            .position(|entry| entry.name == name && entry.owner == owner)
        {
            Some(index) => {
                self.entries.remove(index);
                OK
            }
            None => ERR_NOT_FOUND,
        }
    }

    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|entry| entry.name.clone()).collect()
    }
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());

fn registry() -> MutexGuard<'static, Registry> {
    // A plugin that panicked while holding the lock must not take the whole
    // registry down with it.
    REGISTRY.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Registers an interface in the process wide registry.
pub fn register(owner: &str, name: &str, table: *const ParsecOrbitInterfaceHeader) -> i32 {
    let result = registry().register(owner, name, table);
    match result {
        OK => {
            // Safety: `register` accepted the table, so the header is readable.
            let header = unsafe { *table };
            log(
                LOG_INFO,
                &format!(
                    "{owner} registered interface {name} {}.{} ({} bytes)",
                    header.major(),
                    header.minor(),
                    header.size
                ),
            );
        }
        code => log(
            LOG_DEBUG,
            &format!("{owner} could not register interface {name}: error {code}"),
        ),
    }
    result
}

/// Removes an interface the caller registered.
pub fn unregister(owner: &str, name: &str) -> i32 {
    registry().unregister(owner, name)
}

/// The table registered under `name`, or null.
pub fn find(name: &str) -> *const ParsecOrbitInterfaceHeader {
    registry().find(name)
}

/// Names of all registered interfaces, in registration order.
pub fn registered_names() -> Vec<String> {
    registry().names()
}

// ---- logging --------------------------------------------------------------

static LOGGER: OnceLock<&'static (dyn Logger + Sync)> = OnceLock::new();

/// Sets the sink the host callbacks write to. The first call wins.
pub fn install_logger(logger: &'static (dyn Logger + Sync)) {
    let _ = LOGGER.set(logger);
}

fn log(level: u32, message: &str) {
    if let Some(logger) = LOGGER.get() {
        logger.log(level, message);
    }
}

// ---- host callbacks -------------------------------------------------------

unsafe fn utf8<'a>(pointer: *const u8, len: usize) -> Option<std::borrow::Cow<'a, str>> {
    if pointer.is_null() {
        return None;
    }
    Some(String::from_utf8_lossy(unsafe {
        std::slice::from_raw_parts(pointer, len)
    }))
}

unsafe fn c_str<'a>(pointer: *const u8) -> Option<&'a str> {
    if pointer.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(pointer as *const _) }.to_str().ok()
}

/// The id a host table was made for.
unsafe fn owner_of<'a>(host: *const ParsecOrbitHost) -> Option<&'a str> {
    let host = unsafe { host.as_ref() }?;
    unsafe { c_str(host.plugin_id) }
}

/// `ParsecOrbitHost::log`.
pub unsafe extern "system" fn host_log(level: u32, message: *const u8, len: usize) {
    if let Some(message) = unsafe { utf8(message, len) } {
        log(level, &message);
    }
}

/// `ParsecOrbitHost::log_ex`: the target goes in front of the message.
pub unsafe extern "system" fn host_log_ex(
    level: u32,
    target: *const u8,
    target_len: usize,
    message: *const u8,
    len: usize,
) {
    let Some(message) = (unsafe { utf8(message, len) }) else {
        return;
    };
    match unsafe { utf8(target, target_len) } {
        Some(target) if !target.is_empty() => log(level, &format!("{target}: {message}")),
        _ => log(level, &message),
    }
}

/// `ParsecOrbitHost::query_interface`.
pub unsafe extern "system" fn host_query_interface(name: *const u8) -> *mut c_void {
    let Some(name) = (unsafe { c_str(name) }) else {
        return std::ptr::null_mut();
    };
    let found = find(name);
    if found.is_null() {
        log(LOG_DEBUG, &format!("interface {name} is not registered"));
    }
    found as *mut c_void
}

/// `ParsecOrbitHost::register_interface`.
pub unsafe extern "system" fn host_register_interface(
    host: *const ParsecOrbitHost,
    name: *const u8,
    table: *const ParsecOrbitInterfaceHeader,
) -> i32 {
    let (Some(owner), Some(name)) = (unsafe { owner_of(host) }, unsafe { c_str(name) }) else {
        return ERR_INVALID;
    };
    register(owner, name, table)
}

/// `ParsecOrbitHost::unregister_interface`.
pub unsafe extern "system" fn host_unregister_interface(
    host: *const ParsecOrbitHost,
    name: *const u8,
) -> i32 {
    let (Some(owner), Some(name)) = (unsafe { owner_of(host) }, unsafe { c_str(name) }) else {
        return ERR_INVALID;
    };
    unregister(owner, name)
}

// ---- per-plugin tables ----------------------------------------------------

/// Directory a plugin may keep data in: `<root>\<id>` with every character
/// outside `[A-Za-z0-9._-]` replaced, so an id can never point out of `root`.
pub fn data_dir_for(root: &Path, id: &str) -> PathBuf {
    let mut name: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name.chars().all(|c| c == '.') {
        name = format!("_{name}");
    }
    root.join(name)
}

/// Leaks a NUL-terminated copy of `text` for the lifetime of the process.
pub(crate) fn leak_c_string(text: &str) -> *const u8 {
    let text = text.replace('\0', "?");
    CString::new(text)
        .map(|text| text.into_raw() as *const u8)
        .unwrap_or(std::ptr::null())
}

/// Builds the host table of one plugin. The table and its strings are leaked:
/// a plugin may keep the pointer for as long as the process lives.
pub fn host_for(
    loader_version: &str,
    id: &str,
    dir: &Path,
    data_dir: Option<&Path>,
) -> &'static ParsecOrbitHost {
    Box::leak(Box::new(ParsecOrbitHost {
        log: Some(host_log),
        query_interface: Some(host_query_interface),
        loader_version: leak_c_string(loader_version),
        plugin_id: leak_c_string(id),
        plugin_dir: leak_c_string(&dir.to_string_lossy()),
        data_dir: data_dir.map_or(std::ptr::null(), |dir| leak_c_string(&dir.to_string_lossy())),
        log_ex: Some(host_log_ex),
        register_interface: Some(host_register_interface),
        unregister_interface: Some(host_unregister_interface),
        ..ParsecOrbitHost::blank()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    static HEADER: ParsecOrbitInterfaceHeader = ParsecOrbitInterfaceHeader::new(8, 1, 2);
    static TINY: ParsecOrbitInterfaceHeader = ParsecOrbitInterfaceHeader {
        size: 4,
        version: 0,
    };

    fn table() -> *const ParsecOrbitInterfaceHeader {
        &HEADER
    }

    // ---- names ----

    #[test]
    fn names_are_ascii_dotted_identifiers() {
        for good in [
            "parsec-orbit.webview",
            "parsec-orbit.kind.js",
            "com.example.thing",
            "a",
            "A_b-c.1",
        ] {
            assert!(valid_name(good), "{good}");
        }
        for bad in [
            "",
            ".leading",
            "trailing.",
            "double..dot",
            "has space",
            "nul\0byte",
            "caf\u{e9}",
            "slash/name",
            &"x".repeat(129),
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(valid_name(&"x".repeat(128)));
    }

    // ---- registry ----

    #[test]
    fn a_registered_table_is_found_with_its_owner() {
        let mut registry = Registry::new();
        assert!(registry.find("com.example.svc").is_null());
        assert_eq!(registry.register("com.example.a", "com.example.svc", table()), OK);
        assert_eq!(registry.find("com.example.svc"), table());
        assert_eq!(registry.owner("com.example.svc"), Some("com.example.a"));
        assert_eq!(registry.names(), ["com.example.svc"]);
    }

    #[test]
    fn a_name_can_only_be_registered_once() {
        let mut registry = Registry::new();
        assert_eq!(registry.register("a", "com.example.svc", table()), OK);
        assert_eq!(registry.register("b", "com.example.svc", table()), ERR_EXISTS);
        assert_eq!(registry.owner("com.example.svc"), Some("a"));
    }

    #[test]
    fn invalid_registrations_are_refused() {
        let mut registry = Registry::new();
        assert_eq!(registry.register("a", "bad name", table()), ERR_INVALID);
        assert_eq!(
            registry.register("a", "com.example.svc", std::ptr::null()),
            ERR_INVALID
        );
        assert_eq!(registry.register("a", "com.example.svc", &TINY), ERR_INVALID);
        let misaligned = (table() as usize + 1) as *const ParsecOrbitInterfaceHeader;
        assert_eq!(
            registry.register("a", "com.example.svc", misaligned),
            ERR_INVALID
        );
        assert!(registry.names().is_empty());
    }

    #[test]
    fn the_reserved_prefix_belongs_to_official_plugins_except_for_kinds() {
        let mut registry = Registry::new();
        assert_eq!(
            registry.register("com.example.third", "parsec-orbit.webview", table()),
            ERR_INVALID
        );
        assert_eq!(
            registry.register("com.parsec-orbit.webview-bridge", "parsec-orbit.webview", table()),
            OK
        );
        // A third party that implements a kind registers its handler.
        assert_eq!(
            registry.register("com.example.lua", "parsec-orbit.kind.lua", table()),
            OK
        );
    }

    #[test]
    fn only_the_owner_can_unregister() {
        let mut registry = Registry::new();
        registry.register("a", "com.example.svc", table());
        assert_eq!(registry.unregister("b", "com.example.svc"), ERR_NOT_FOUND);
        assert!(!registry.find("com.example.svc").is_null());
        assert_eq!(registry.unregister("a", "com.example.svc"), OK);
        assert!(registry.find("com.example.svc").is_null());
        assert_eq!(registry.unregister("a", "com.example.svc"), ERR_NOT_FOUND);
        // The name is free again.
        assert_eq!(registry.register("b", "com.example.svc", table()), OK);
    }

    // ---- data directory ----

    #[test]
    fn data_dir_is_the_sanitised_id_below_the_root() {
        let root = Path::new("root");
        assert_eq!(
            data_dir_for(root, "com.example.p"),
            root.join("com.example.p")
        );
        assert_eq!(data_dir_for(root, "a/b:c d"), root.join("a_b_c_d"));
        assert_eq!(data_dir_for(root, ".."), root.join("_.."));
        assert_eq!(data_dir_for(root, ""), root.join("_"));
        assert_eq!(
            data_dir_for(root, "C:\\x\\y.dll"),
            root.join("C__x_y.dll")
        );
    }

    // ---- host tables ----

    #[test]
    fn a_host_table_carries_the_identity_of_its_plugin() {
        let host = host_for(
            "1.2.3",
            "com.example.p",
            Path::new("plugins/p"),
            Some(Path::new("data/p")),
        );
        assert_eq!(host.abi_version, parsec_orbit_plugin_abi::ABI_VERSION);
        assert_eq!(
            host.struct_size as usize,
            std::mem::size_of::<ParsecOrbitHost>()
        );
        assert_eq!(host.loader_version(), Some("1.2.3"));
        assert_eq!(host.plugin_id(), Some("com.example.p"));
        assert_eq!(host.plugin_dir(), Some("plugins/p"));
        assert_eq!(host.data_dir(), Some("data/p"));
        assert!(host.log.is_some() && host.log_ex.is_some());
        assert!(host.query_interface.is_some());
        assert!(host.register_interface.is_some() && host.unregister_interface.is_some());
        assert_eq!(host.reserved, [0; 8]);
        assert_eq!(host.reserved2, [0; 8]);
    }

    #[test]
    fn every_plugin_gets_a_table_of_its_own() {
        let a = host_for("1", "a", Path::new("a"), None);
        let b = host_for("1", "b", Path::new("b"), None);
        assert!(!std::ptr::eq(a, b));
        assert_eq!(a.plugin_id(), Some("a"));
        assert_eq!(b.plugin_id(), Some("b"));
        assert_eq!(a.data_dir(), None);
    }

    #[test]
    fn interior_nuls_cannot_truncate_or_break_a_string() {
        let host = host_for("1", "bad\0id", Path::new("d"), None);
        assert_eq!(host.plugin_id(), Some("bad?id"));
    }

    // ---- the callbacks, through a table ----

    static SEEN: StdMutex<Vec<(u32, String)>> = StdMutex::new(Vec::new());

    struct Recorder;
    impl Logger for Recorder {
        fn log(&self, level: u32, message: &str) {
            SEEN.lock().unwrap().push((level, message.to_string()));
        }
    }
    static RECORDER: Recorder = Recorder;

    fn seen(text: &str) -> bool {
        SEEN.lock().unwrap().iter().any(|(_, line)| line.contains(text))
    }

    #[test]
    fn plugins_register_and_find_interfaces_through_their_own_table() {
        install_logger(&RECORDER);
        let owner = host_for("1", "com.example.cb-owner", Path::new("."), None);
        let other = host_for("1", "com.example.cb-other", Path::new("."), None);

        assert_eq!(
            owner.register(c"com.example.cb.svc", &HEADER),
            OK,
            "registration through the SDK helper"
        );
        let found = unsafe { owner.interface(c"com.example.cb.svc") }.unwrap();
        assert_eq!((found.major(), found.minor()), (1, 2));
        assert!(unsafe { other.interface(c"com.example.cb.svc") }.is_some());
        assert!(unsafe { other.interface(c"com.example.cb.missing") }.is_none());
        assert!(seen("com.example.cb-owner registered interface com.example.cb.svc 1.2"));

        assert_eq!(other.register(c"com.example.cb.svc", &HEADER), ERR_EXISTS);

        let unregister = other.unregister_interface.unwrap();
        assert_eq!(
            unsafe { unregister(other, c"com.example.cb.svc".as_ptr() as *const u8) },
            ERR_NOT_FOUND,
            "somebody else's name"
        );
        let unregister = owner.unregister_interface.unwrap();
        assert_eq!(
            unsafe { unregister(owner, c"com.example.cb.svc".as_ptr() as *const u8) },
            OK
        );
        assert!(unsafe { owner.interface(c"com.example.cb.svc") }.is_none());
    }

    #[test]
    fn callbacks_reject_missing_arguments() {
        let host = host_for("1", "com.example.args", Path::new("."), None);
        unsafe {
            assert_eq!(
                host_register_interface(std::ptr::null(), c"x.y".as_ptr() as *const u8, table()),
                ERR_INVALID
            );
            assert_eq!(
                host_register_interface(host, std::ptr::null(), table()),
                ERR_INVALID
            );
            assert_eq!(
                host_unregister_interface(host, std::ptr::null()),
                ERR_INVALID
            );
            assert!(host_query_interface(std::ptr::null()).is_null());
            host_log(2, std::ptr::null(), 4);
            host_log_ex(2, std::ptr::null(), 0, std::ptr::null(), 0);
        }
    }

    #[test]
    fn log_and_log_ex_reach_the_installed_logger() {
        install_logger(&RECORDER);
        let host = host_for("1", "com.example.logger", Path::new("."), None);
        host.log(2, "plain line");
        host.log_with_target(3, "com.example.logger", "line with a target");
        host.log_with_target(3, "", "line without a target");
        assert!(seen("plain line"));
        assert!(SEEN.lock().unwrap().contains(&(
            3,
            "com.example.logger: line with a target".to_string()
        )));
        assert!(SEEN
            .lock()
            .unwrap()
            .contains(&(3, "line without a target".to_string())));
        // Not UTF-8: replaced, never a panic.
        unsafe { host_log(2, [0xffu8, 0xfe].as_ptr(), 2) };
    }
}
