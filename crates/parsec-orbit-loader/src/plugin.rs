//! Win32 side of plugin loading.
//!
//! Which plugins to load, in what order and whether their descriptors are
//! acceptable is decided in `parsec-orbit-core`. This module supplies what the
//! core cannot: mapping a DLL ([`Win32Platform`]), the log sink ([`FileLogger`])
//! and the host table handed to plugins.

use std::ffi::{c_void, CStr};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use parsec_orbit_core::{Env, Exports, Loaded, Logger, Module, Platform};
use parsec_orbit_plugin_abi::{
    InitFn, LegacyInitFn, ParsecOrbitHost, QueryFn, ShutdownFn, ABI_VERSION, EXPORT_INIT,
    EXPORT_LEGACY_INIT, EXPORT_QUERY, EXPORT_SHUTDOWN, HOST_REVISION_1_SIZE,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LoadLibraryW};

pub use parsec_orbit_core::Summary;

use crate::config::Config;
use crate::log;

const LOADER_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");

struct HostCell(ParsecOrbitHost);

// Safety: the host table is immutable for the lifetime of the process.
unsafe impl Sync for HostCell {}

// Revision 2 is switched on once the per-plugin tables exist; until then the
// shared table advertises revision 1 only.
static HOST: HostCell = HostCell(ParsecOrbitHost {
    struct_size: HOST_REVISION_1_SIZE as u32,
    log: Some(host_log),
    query_interface: Some(query_interface),
    loader_version: LOADER_VERSION.as_ptr(),
    ..ParsecOrbitHost::blank()
});

// Safety: the registry is only touched through its mutex, and a module handle
// is valid process wide.
static REGISTRY: OnceLock<Mutex<Vec<Loaded>>> = OnceLock::new();

fn remember(plugins: Vec<Loaded>) {
    let registry = REGISTRY.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut loaded) = registry.lock() {
        loaded.extend(plugins);
    }
}

/// Ids of the plugins mapped so far, in load order.
#[allow(dead_code)]
pub fn loaded() -> Vec<String> {
    REGISTRY
        .get()
        .and_then(|registry| {
            registry
                .lock()
                .ok()
                .map(|plugins| plugins.iter().map(|plugin| plugin.id.clone()).collect())
        })
        .unwrap_or_default()
}

unsafe extern "system" fn host_log(level: u32, message: *const u8, len: usize) {
    if message.is_null() {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(message, len) };
    log::by_level(level, &String::from_utf8_lossy(bytes));
}

/// Extension point: plugins ask for optional host interfaces here. Nothing is
/// served yet; the names are reserved for the WebView bridge, the hook API and
/// the JavaScript runtime.
unsafe extern "system" fn query_interface(name: *const u8) -> *mut c_void {
    if !name.is_null() {
        let name = unsafe { CStr::from_ptr(name as *const _) };
        log::debug(&format!(
            "plugin asked for the optional interface {:?} (reserved, not implemented yet)",
            name.to_string_lossy()
        ));
    }
    std::ptr::null_mut()
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`: resolve
/// a plugin's dependencies in its own directory and in System32 only. The
/// client restricts the process-wide default search path early in its
/// start-up, so the flagless attempt can fail with `ERROR_MOD_NOT_FOUND` even
/// though the plugin itself is right there.
const PLUGIN_LOAD_FLAGS: u32 = 0x0000_0100 | 0x0000_0800;

/// Maps libraries with `LoadLibrary` and resolves the plugin exports by name.
struct Win32Platform;

impl Platform for Win32Platform {
    fn load(&self, path: &Path) -> Result<Module, String> {
        let name = wide(path);
        let mut module = unsafe { LoadLibraryW(name.as_ptr()) };
        if module.is_null() {
            let first = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            module =
                unsafe { LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), PLUGIN_LOAD_FLAGS) };
            if module.is_null() {
                let second = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                return Err(format!(
                    "could not load {} (LoadLibraryW error {first}, LoadLibraryExW error {second})",
                    path.display()
                ));
            }
        }

        let symbol = |symbol: &str| {
            let symbol = format!("{symbol}\0");
            unsafe { GetProcAddress(module, symbol.as_ptr()) }
        };

        // Safety: the exports are looked up by the names the ABI reserves for
        // these signatures.
        let exports = unsafe {
            Exports {
                query: symbol(EXPORT_QUERY).map(|f| std::mem::transmute::<_, QueryFn>(f)),
                init: symbol(EXPORT_INIT).map(|f| std::mem::transmute::<_, InitFn>(f)),
                shutdown: symbol(EXPORT_SHUTDOWN).map(|f| std::mem::transmute::<_, ShutdownFn>(f)),
                legacy_init: symbol(EXPORT_LEGACY_INIT)
                    .map(|f| std::mem::transmute::<_, LegacyInitFn>(f)),
            }
        };
        Ok(Module {
            handle: module as usize,
            exports,
        })
    }
}

/// Sends the core's lines to the loader log.
struct FileLogger;

impl Logger for FileLogger {
    fn log(&self, level: u32, message: &str) {
        log::by_level(level, message);
    }
}

/// Walks every configured plugin directory.
pub fn scan(config: &Config) -> Summary {
    let result = parsec_orbit_core::scan(
        Env {
            platform: &Win32Platform,
            logger: &FileLogger,
            host: &HOST.0,
        },
        &config.plugin_dirs,
    );
    remember(result.loaded);
    result.summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use parsec_orbit_core::{KIND_DLL, KIND_JS, MANIFEST};
    use std::path::PathBuf;

    // ---- host table -------------------------------------------------------

    #[test]
    fn host_table_describes_this_loader() {
        let host = &HOST.0;
        assert_eq!(host.abi_version, ABI_VERSION);
        assert_eq!(host.struct_size as usize, HOST_REVISION_1_SIZE);
        assert!(host.log.is_some());
        assert!(host.query_interface.is_some());
        assert_eq!(host.reserved, [0; 8]);
        assert_eq!(host.loader_version(), Some(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn host_log_tolerates_a_null_message_and_empty_text() {
        unsafe {
            host_log(2, std::ptr::null(), 10);
            host_log(2, b"".as_ptr(), 0);
            host_log(99, b"unknown level".as_ptr(), 13);
            host_log(2, [0xffu8, 0xfe].as_ptr(), 2);
        }
    }

    #[test]
    fn query_interface_serves_nothing_yet() {
        unsafe {
            assert!(query_interface(std::ptr::null()).is_null());
            assert!(query_interface(b"parsec-orbit.hooks\0".as_ptr()).is_null());
            assert!(query_interface(b"\0".as_ptr()).is_null());
        }
    }

    // ---- Win32 platform ---------------------------------------------------

    #[test]
    fn load_reports_a_missing_file_with_both_error_codes() {
        let dir = TempDir::new("load-missing");
        let error = Win32Platform
            .load(&dir.path().join("nope.dll"))
            .err()
            .expect("a missing file cannot load");
        assert!(error.contains("could not load"), "{error}");
        assert!(error.contains("LoadLibraryW error"), "{error}");
        assert!(error.contains("LoadLibraryExW error"), "{error}");
    }

    #[test]
    fn load_rejects_a_file_that_is_not_a_pe_image() {
        let dir = TempDir::new("load-garbage");
        let path = dir.write("garbage.dll", "this is not a dll");
        assert!(Win32Platform.load(&path).is_err());
    }

    #[test]
    fn load_of_a_dll_without_plugin_exports_resolves_none() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let path = PathBuf::from(root).join("System32").join("kernel32.dll");
        let module = Win32Platform
            .load(&path)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_ne!(module.handle, 0);
        assert!(module.exports.query.is_none());
        assert!(module.exports.init.is_none());
        assert!(module.exports.shutdown.is_none());
        assert!(module.exports.legacy_init.is_none());
    }

    // ---- scanning with the real platform ----------------------------------

    fn config_for(dirs: &[&Path]) -> Config {
        Config {
            log_candidates: Vec::new(),
            plugin_dirs: dirs.iter().map(|dir| dir.to_path_buf()).collect(),
            devtools_port: None,
            any_host: false,
        }
    }

    #[test]
    fn scan_with_the_real_platform_never_loads_garbage() {
        let dir = TempDir::new("scan-real");
        dir.write(
            "js/plugin.toml",
            "id = \"js.one\"\nkind = \"js\"\nentry = \"main.js\"\n",
        );
        dir.write(
            "garbage/plugin.toml",
            "id = \"garbage\"\nentry = \"x.dll\"\n",
        );
        dir.write("garbage/x.dll", "not a dll");
        dir.write("loose.dll", "not a dll");

        let summary = scan(&config_for(&[dir.path()]));
        assert_eq!((summary.loaded, summary.skipped, summary.failed), (0, 1, 2));
        assert!(loaded().iter().all(|id| id != "garbage"));
    }

    #[test]
    fn the_core_constants_are_the_documented_ones() {
        assert_eq!(KIND_DLL, "dll");
        assert_eq!(KIND_JS, "js");
        assert_eq!(MANIFEST, "plugin.toml");
    }
}
