//! Plugin discovery and loading.
//!
//! The loader understands two layouts:
//!
//! ```text
//! <client dir>\plugins\<name>\plugin.toml   + entry (dll/js)
//! %APPDATA%\ParsecOrbit\plugins\<name>\plugin.toml
//! <client dir>\plugins\loose.dll            (no manifest, legacy layout)
//! ```
//!
//! `plugin.toml` selects the plugin kind. `dll` plugins are mapped and called
//! through the ABI in `parsec-orbit-plugin-sdk`; `js` plugins are already part of
//! the schema but need the JavaScript runtime that is not written yet, so they
//! are reported and skipped instead of being loaded.

use std::collections::HashSet;
use std::ffi::{c_void, CStr};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use parsec_orbit_plugin_sdk::{
    InitFn, LegacyInitFn, ParsecOrbitHost, ParsecOrbitPluginDesc, QueryFn, ShutdownFn, ABI_VERSION,
    EXPORT_INIT, EXPORT_LEGACY_INIT, EXPORT_QUERY, EXPORT_SHUTDOWN,
};
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use crate::config::Config;
use crate::log;

/// File name of the plugin manifest inside a plugin directory.
pub const MANIFEST: &str = "plugin.toml";

/// Plugin kinds named in the manifest.
pub const KIND_DLL: &str = "dll";
pub const KIND_JS: &str = "js";

const LOADER_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");

struct HostCell(ParsecOrbitHost);

// Safety: the host table is immutable for the lifetime of the process.
unsafe impl Sync for HostCell {}

static HOST: HostCell = HostCell(ParsecOrbitHost {
    abi_version: ABI_VERSION,
    struct_size: std::mem::size_of::<ParsecOrbitHost>() as u32,
    log: Some(host_log),
    query_interface: Some(query_interface),
    loader_version: LOADER_VERSION.as_ptr(),
    reserved: [0; 8],
});

/// Metadata read from `plugin.toml`. Every field is optional except the ones
/// the loader needs to pick an entry point; missing values fall back to
/// defaults that keep a hand-written plugin working.
#[derive(Debug, Default, Clone)]
pub struct Manifest {
    pub id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub kind: Option<String>,
    pub entry: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub license: Option<String>,
    pub api: u32,
    pub enabled: bool,
}

#[derive(Default)]
pub struct Summary {
    pub loaded: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// A mapped plugin. Kept so a future version can unload or reload plugins
/// without restarting the client.
#[allow(dead_code)]
struct Loaded {
    id: String,
    module: HMODULE,
    shutdown: Option<ShutdownFn>,
}

// Safety: the registry is only touched through its mutex, and a module handle
// is valid process wide.
unsafe impl Send for Loaded {}

static REGISTRY: OnceLock<Mutex<Vec<Loaded>>> = OnceLock::new();

fn remember(plugin: Loaded) {
    let registry = REGISTRY.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut plugins) = registry.lock() {
        plugins.push(plugin);
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

unsafe fn text(pointer: *const u8) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(pointer as *const _) }
        .to_str()
        .ok()
        .map(str::to_string)
}

/// Parses `plugin.toml`.
pub fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let source = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let table: toml::Table = toml::from_str(&source).map_err(|error| error.to_string())?;

    let string = |key: &str| {
        table
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
    };

    Ok(Manifest {
        id: string("id"),
        name: string("name"),
        version: string("version"),
        kind: string("kind"),
        entry: string("entry"),
        author: string("author"),
        description: string("description"),
        license: string("license"),
        api: table
            .get("api")
            .and_then(|value| value.as_integer())
            .unwrap_or(ABI_VERSION as i64) as u32,
        enabled: table
            .get("enabled")
            .and_then(|value| value.as_bool())
            .unwrap_or(true),
    })
}

fn describe(manifest: Option<&Manifest>, path: &Path) -> String {
    match manifest {
        Some(manifest) => format!(
            "{} {}",
            manifest
                .id
                .clone()
                .unwrap_or_else(|| path.display().to_string()),
            manifest.version.clone().unwrap_or_else(|| "-".into())
        ),
        None => path.display().to_string(),
    }
}

/// Maps one DLL, validates its descriptor and runs its entry points.
///
/// Returns the plugin id on success.
fn load_dll(path: &Path, manifest: Option<&Manifest>) -> Result<String, String> {
    let name = wide(path);
    let module = unsafe { LoadLibraryW(name.as_ptr()) };
    if module.is_null() {
        return Err(format!(
            "LoadLibraryW failed with Win32 error {}",
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
        ));
    }

    let symbol = |symbol: &str| {
        let symbol = format!("{symbol}\0");
        unsafe { GetProcAddress(module, symbol.as_ptr()) }
    };

    let label = describe(manifest, path);

    let Some(query) = symbol(EXPORT_QUERY) else {
        // Plugin from before the descriptor existed: single legacy entry point.
        let Some(legacy) = symbol(EXPORT_LEGACY_INIT) else {
            return Err(format!("no {EXPORT_QUERY} or {EXPORT_LEGACY_INIT} export"));
        };
        let legacy: LegacyInitFn = unsafe { std::mem::transmute(legacy) };
        let result = unsafe { legacy(std::ptr::null_mut()) };
        log::warn(&format!(
            "{label}: legacy entry point, {EXPORT_LEGACY_INIT} returned {result}"
        ));
        let id = path.display().to_string();
        remember(Loaded {
            id: id.clone(),
            module,
            shutdown: None,
        });
        return Ok(id);
    };

    let query: QueryFn = unsafe { std::mem::transmute(query) };
    let descriptor: *const ParsecOrbitPluginDesc = unsafe { query(ABI_VERSION) };
    if descriptor.is_null() {
        return Err("query returned no descriptor".to_string());
    }
    let descriptor = unsafe { &*descriptor };

    let struct_size = descriptor.struct_size as usize;
    let expected = std::mem::size_of::<ParsecOrbitPluginDesc>();
    if struct_size < expected {
        return Err(format!(
            "descriptor is {struct_size} bytes, expected at least {expected}"
        ));
    }
    if descriptor.abi_version > ABI_VERSION {
        return Err(format!(
            "plugin needs ABI {} but this loader implements {ABI_VERSION}",
            descriptor.abi_version
        ));
    }

    let id = unsafe { text(descriptor.id) }.unwrap_or_else(|| path.display().to_string());
    let version = unsafe { text(descriptor.version) }.unwrap_or_else(|| "-".to_string());
    let display = unsafe { text(descriptor.name) }.unwrap_or_default();
    let author = unsafe { text(descriptor.author) }.unwrap_or_default();
    let description = unsafe { text(descriptor.description) }.unwrap_or_default();
    let license = unsafe { text(descriptor.license) }.unwrap_or_default();

    let Some(init) = symbol(EXPORT_INIT) else {
        return Err(format!("descriptor found but {EXPORT_INIT} is missing"));
    };
    let init: InitFn = unsafe { std::mem::transmute(init) };
    let result = unsafe { init(&HOST.0 as *const ParsecOrbitHost) };

    let shutdown: Option<ShutdownFn> = symbol(EXPORT_SHUTDOWN)
        .map(|pointer| unsafe { std::mem::transmute::<_, ShutdownFn>(pointer) });

    log::info(&format!(
        "loaded {id} {version} ({display}) ABI {} by {author} [{license}] - {description} - init returned {result}",
        descriptor.abi_version
    ));

    remember(Loaded {
        id: id.clone(),
        module,
        shutdown,
    });

    if result != 0 {
        return Err(format!("init returned {result}"));
    }
    Ok(id)
}

fn first_dll(dir: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
        })
        .collect();
    candidates.sort();
    candidates.into_iter().next()
}

/// Handles one plugin directory.
fn load_plugin_dir(dir: &Path, summary: &mut Summary, seen: &mut HashSet<String>) {
    let manifest_path = dir.join(MANIFEST);
    if !manifest_path.is_file() {
        log::debug(&format!("{}: no {MANIFEST}, skipped", dir.display()));
        summary.skipped += 1;
        return;
    }

    let manifest = match read_manifest(&manifest_path) {
        Ok(manifest) => manifest,
        Err(error) => {
            log::error(&format!("{}: {error}", manifest_path.display()));
            summary.failed += 1;
            return;
        }
    };

    let label = describe(Some(&manifest), dir);

    log::debug(&format!(
        "{label}: name={:?} author={:?} license={:?} description={:?}",
        manifest.name, manifest.author, manifest.license, manifest.description
    ));

    if !manifest.enabled {
        log::info(&format!("{label}: disabled in the manifest, skipped"));
        summary.skipped += 1;
        return;
    }
    if manifest.api > ABI_VERSION {
        log::warn(&format!(
            "{label}: manifest asks for API {} but the loader implements {ABI_VERSION}",
            manifest.api
        ));
        summary.failed += 1;
        return;
    }
    if let Some(id) = &manifest.id {
        if !seen.insert(id.clone()) {
            log::debug(&format!("{label}: already loaded, skipped"));
            summary.skipped += 1;
            return;
        }
    }

    let kind = manifest
        .kind
        .clone()
        .unwrap_or_else(|| KIND_DLL.to_string());
    match kind.as_str() {
        KIND_DLL => {
            let entry = manifest
                .entry
                .as_ref()
                .map(|entry| dir.join(entry))
                .or_else(|| first_dll(dir));
            let Some(entry) = entry else {
                log::warn(&format!("{label}: no DLL entry point found"));
                summary.failed += 1;
                return;
            };
            match load_dll(&entry, Some(&manifest)) {
                Ok(_) => summary.loaded += 1,
                Err(error) => {
                    log::error(&format!("{label}: {error}"));
                    summary.failed += 1;
                }
            }
        }
        KIND_JS => {
            // Reserved: the JavaScript runtime (planned as an embedded Boa
            // engine handed out through `query_interface("parsec-orbit.js")`) is
            // not written yet, so the plugin stays on disk and is reported.
            log::info(&format!(
                "{label}: kind \"{KIND_JS}\" needs the JavaScript runtime, which is not implemented yet"
            ));
            summary.skipped += 1;
        }
        other => {
            log::warn(&format!("{label}: unknown kind \"{other}\""));
            summary.failed += 1;
        }
    }
}

/// Walks every configured plugin directory.
pub fn scan(config: &Config) -> Summary {
    let mut summary = Summary::default();
    let mut seen: HashSet<String> = HashSet::new();

    for dir in &config.plugin_dirs {
        if !dir.is_dir() {
            log::debug(&format!("{}: not present", dir.display()));
            continue;
        }
        log::debug(&format!("scanning {}", dir.display()));

        let mut entries: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect(),
            Err(error) => {
                log::warn(&format!("{}: {error}", dir.display()));
                continue;
            }
        };
        entries.sort();

        for entry in entries {
            if entry.is_dir() {
                load_plugin_dir(&entry, &mut summary, &mut seen);
                continue;
            }
            let is_dll = entry
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"));
            if !is_dll {
                continue;
            }
            // A DLL dropped straight into a plugin directory is loaded without
            // metadata, which is what the first proof of concept did.
            match load_dll(&entry, None) {
                Ok(_) => summary.loaded += 1,
                Err(error) => {
                    log::error(&format!("{}: {error}", entry.display()));
                    summary.failed += 1;
                }
            }
        }
    }

    summary
}
