//! Win32 side of plugin loading.
//!
//! Which plugins to load, in what order and whether their descriptors are
//! acceptable is decided in `parsec-orbit-core`. This module supplies what the
//! core cannot: mapping a DLL ([`Win32Platform`]), the log sink ([`FileLogger`])
//! and the per-plugin host tables come from `parsec_orbit_core::host`.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use parsec_orbit_core::{Env, Exports, Level, Logger, Module, Platform, Session};
use parsec_orbit_plugin_abi::{
    InitFn, LegacyInitFn, QueryFn, ShutdownFn, EXPORT_INIT, EXPORT_LEGACY_INIT, EXPORT_QUERY,
    EXPORT_SHUTDOWN,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LoadLibraryW};

pub use parsec_orbit_core::Summary;

use crate::config::Config;
use crate::log;

const LOADER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The modules mapped so far; there is no unload, so it lives as long as the
/// process.
static SESSION: Session = Session::new();

/// Ids of the plugins mapped so far, in load order.
#[allow(dead_code)]
pub fn loaded() -> Vec<String> {
    SESSION.loaded_ids()
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

    fn native_kind(&self) -> &'static str {
        parsec_orbit_core::KIND_DLL
    }
}

/// Sends the core's lines to the loader log.
struct FileLogger;

impl Logger for FileLogger {
    fn log(&self, level: Level, message: &str) {
        log::log(level, message);
    }
}

static FILE_LOGGER: FileLogger = FileLogger;

/// Walks every configured plugin directory and logs the `ready:` summary.
pub fn scan(config: &Config) -> Summary {
    parsec_orbit_core::host::install_logger(&FILE_LOGGER);
    SESSION.scan(
        Env {
            platform: &Win32Platform,
            logger: &FILE_LOGGER,
            loader_version: LOADER_VERSION,
            data_root: config.data_root.as_deref(),
        },
        &config.plugin_dirs,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use parsec_orbit_core::{KIND_DLL, KIND_JS, MANIFEST};
    use std::path::PathBuf;

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
            data_root: None,
            devtools_port: None,
            any_host: false,
            console: parsec_orbit_core::console::Target::Off,
            parsec_logs: Vec::new(),
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
