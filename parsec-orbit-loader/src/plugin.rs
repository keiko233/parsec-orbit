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
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LoadLibraryW};

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
    let document = toml::de::DeTable::parse(&source).map_err(|error| error.to_string())?;
    let table = document.get_ref();

    let string = |key: &str| {
        table
            .get(key)
            .and_then(|value| value.get_ref().as_str())
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
            .and_then(|value| value.get_ref().as_integer())
            .and_then(|value| value.as_str().parse::<i64>().ok())
            .unwrap_or(ABI_VERSION as i64) as u32,
        enabled: table
            .get("enabled")
            .and_then(|value| value.get_ref().as_bool())
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

/// `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`: resolve
/// a plugin's dependencies in its own directory and in System32 only. The
/// client restricts the process-wide default search path early in its
/// start-up, so the flagless attempt can fail with `ERROR_MOD_NOT_FOUND` even
/// though the plugin itself is right there.
const PLUGIN_LOAD_FLAGS: u32 = 0x0000_0100 | 0x0000_0800;

/// Maps one DLL, validates its descriptor and runs its entry points.
///
/// Returns the plugin id on success.
fn load_dll(path: &Path, manifest: Option<&Manifest>) -> Result<String, String> {
    let name = wide(path);
    let mut module = unsafe { LoadLibraryW(name.as_ptr()) };
    if module.is_null() {
        let first = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        module = unsafe { LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), PLUGIN_LOAD_FLAGS) };
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
    activate(path, manifest, module, exports, &HOST.0)
}

/// Entry points found in a mapped plugin.
struct Exports {
    query: Option<QueryFn>,
    init: Option<InitFn>,
    shutdown: Option<ShutdownFn>,
    legacy_init: Option<LegacyInitFn>,
}

/// Validates the descriptor of a mapped plugin and runs its entry points.
///
/// Split from [`load_dll`] so the decisions made here do not depend on a real
/// DLL being on disk.
fn activate(
    path: &Path,
    manifest: Option<&Manifest>,
    module: HMODULE,
    exports: Exports,
    host: &ParsecOrbitHost,
) -> Result<String, String> {
    let label = describe(manifest, path);

    let Some(query) = exports.query else {
        // Plugin from before the descriptor existed: single legacy entry point.
        let Some(legacy) = exports.legacy_init else {
            return Err(format!("no {EXPORT_QUERY} or {EXPORT_LEGACY_INIT} export"));
        };
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

    let Some(init) = exports.init else {
        return Err(format!("descriptor found but {EXPORT_INIT} is missing"));
    };
    let result = unsafe { init(host as *const ParsecOrbitHost) };

    log::info(&format!(
        "loaded {id} {version} ({display}) ABI {} by {author} [{license}] - {description} - init returned {result}",
        descriptor.abi_version
    ));

    remember(Loaded {
        id: id.clone(),
        module,
        shutdown: exports.shutdown,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---- manifest -------------------------------------------------------

    fn manifest_of(toml: &str) -> Result<Manifest, String> {
        let dir = TempDir::new("manifest");
        let path = dir.write(MANIFEST, toml);
        read_manifest(&path)
    }

    #[test]
    fn manifest_reads_every_field() {
        let manifest = manifest_of(
            r#"
            id = "com.example.full"
            name = "Full"
            version = "1.2.3"
            kind = "dll"
            entry = "full.dll"
            author = "Someone"
            description = "Everything set"
            license = "MIT"
            api = 1
            enabled = false
            "#,
        )
        .unwrap();
        assert_eq!(manifest.id.as_deref(), Some("com.example.full"));
        assert_eq!(manifest.name.as_deref(), Some("Full"));
        assert_eq!(manifest.version.as_deref(), Some("1.2.3"));
        assert_eq!(manifest.kind.as_deref(), Some("dll"));
        assert_eq!(manifest.entry.as_deref(), Some("full.dll"));
        assert_eq!(manifest.author.as_deref(), Some("Someone"));
        assert_eq!(manifest.description.as_deref(), Some("Everything set"));
        assert_eq!(manifest.license.as_deref(), Some("MIT"));
        assert_eq!(manifest.api, 1);
        assert!(!manifest.enabled);
    }

    #[test]
    fn manifest_defaults_match_the_documented_ones() {
        let manifest = manifest_of("").unwrap();
        assert_eq!(manifest.id, None);
        assert_eq!(manifest.kind, None);
        assert_eq!(manifest.entry, None);
        assert_eq!(manifest.api, ABI_VERSION);
        assert!(manifest.enabled);
    }

    #[test]
    fn manifest_ignores_values_of_the_wrong_type() {
        let manifest = manifest_of("id = 5\nname = true\nenabled = \"no\"\napi = \"1\"\n").unwrap();
        assert_eq!(manifest.id, None);
        assert_eq!(manifest.name, None);
        assert!(manifest.enabled, "a non-bool `enabled` keeps the default");
        assert_eq!(manifest.api, ABI_VERSION);
    }

    #[test]
    fn manifest_with_a_negative_api_is_not_accepted_as_compatible() {
        let manifest = manifest_of("api = -1\n").unwrap();
        assert!(manifest.api > ABI_VERSION);
    }

    #[test]
    fn manifest_errors_are_reported_not_panicked() {
        assert!(manifest_of("id = ").is_err());
        assert!(manifest_of("[unterminated").is_err());
        let dir = TempDir::new("manifest-missing");
        assert!(read_manifest(&dir.path().join(MANIFEST)).is_err());
    }

    #[test]
    fn describe_prefers_the_manifest_id_and_version() {
        let path = Path::new("C:\\plugins\\x");
        let manifest = Manifest {
            id: Some("com.example.x".into()),
            version: Some("2.0.0".into()),
            ..Manifest::default()
        };
        assert_eq!(describe(Some(&manifest), path), "com.example.x 2.0.0");

        let bare = Manifest::default();
        assert_eq!(describe(Some(&bare), path), "C:\\plugins\\x -");
        assert_eq!(describe(None, path), "C:\\plugins\\x");
    }

    // ---- first_dll --------------------------------------------------------

    #[test]
    fn first_dll_is_the_alphabetically_first_dll_of_any_case() {
        let dir = TempDir::new("first-dll");
        dir.write("zeta.dll", "");
        dir.write("Beta.DLL", "");
        dir.write("alpha.txt", "");
        dir.write("alpha.dll.bak", "");
        // Uppercase sorts before lowercase, which is what `sort` gives; the
        // point is that the choice is deterministic.
        assert_eq!(first_dll(dir.path()), Some(dir.path().join("Beta.DLL")));
    }

    #[test]
    fn first_dll_is_none_without_a_dll_or_a_directory() {
        let dir = TempDir::new("first-dll-none");
        dir.write("readme.txt", "");
        assert_eq!(first_dll(dir.path()), None);
        assert_eq!(first_dll(&dir.path().join("missing")), None);
    }

    // ---- host table -------------------------------------------------------

    #[test]
    fn host_table_describes_this_loader() {
        let host = &HOST.0;
        assert_eq!(host.abi_version, ABI_VERSION);
        assert_eq!(
            host.struct_size as usize,
            std::mem::size_of::<ParsecOrbitHost>()
        );
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

    // ---- activate ---------------------------------------------------------

    static ID: &[u8] = b"com.example.fake\0";
    static NAME: &[u8] = b"Fake\0";
    static VERSION: &[u8] = b"3.2.1\0";
    static OTHER: &[u8] = b"other\0";

    fn descriptor() -> ParsecOrbitPluginDesc {
        ParsecOrbitPluginDesc {
            abi_version: ABI_VERSION,
            struct_size: std::mem::size_of::<ParsecOrbitPluginDesc>() as u32,
            id: ID.as_ptr(),
            name: NAME.as_ptr(),
            version: VERSION.as_ptr(),
            author: OTHER.as_ptr(),
            description: OTHER.as_ptr(),
            license: OTHER.as_ptr(),
            capabilities: 0,
        }
    }

    struct Shared(ParsecOrbitPluginDesc);
    unsafe impl Sync for Shared {}
    unsafe impl Send for Shared {}

    macro_rules! query_returning {
        ($name:ident, $desc:expr) => {
            unsafe extern "system" fn $name(_abi: u32) -> *const ParsecOrbitPluginDesc {
                static DESC: std::sync::OnceLock<Shared> = std::sync::OnceLock::new();
                &DESC.get_or_init(|| Shared($desc)).0
            }
        };
    }

    query_returning!(query_valid, descriptor());
    query_returning!(
        query_small,
        ParsecOrbitPluginDesc {
            struct_size: std::mem::size_of::<ParsecOrbitPluginDesc>() as u32 - 8,
            ..descriptor()
        }
    );
    query_returning!(
        query_newer,
        ParsecOrbitPluginDesc {
            abi_version: ABI_VERSION + 1,
            ..descriptor()
        }
    );
    query_returning!(
        query_grown,
        ParsecOrbitPluginDesc {
            // A newer SDK of the same ABI appended fields; that must be fine.
            struct_size: std::mem::size_of::<ParsecOrbitPluginDesc>() as u32 + 64,
            ..descriptor()
        }
    );
    query_returning!(
        query_anonymous,
        ParsecOrbitPluginDesc {
            id: std::ptr::null(),
            version: std::ptr::null(),
            name: std::ptr::null(),
            ..descriptor()
        }
    );

    unsafe extern "system" fn query_null(_abi: u32) -> *const ParsecOrbitPluginDesc {
        std::ptr::null()
    }

    static INIT_CALLS: AtomicUsize = AtomicUsize::new(0);
    static SEEN_HOST_ABI: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "system" fn init_ok(host: *const ParsecOrbitHost) -> i32 {
        INIT_CALLS.fetch_add(1, Ordering::SeqCst);
        SEEN_HOST_ABI.store(unsafe { (*host).abi_version } as usize, Ordering::SeqCst);
        0
    }

    unsafe extern "system" fn init_fails(_host: *const ParsecOrbitHost) -> i32 {
        3
    }

    static LEGACY_ARG_WAS_NULL: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "system" fn legacy(argument: *mut c_void) -> u32 {
        LEGACY_ARG_WAS_NULL.store(argument.is_null() as usize + 1, Ordering::SeqCst);
        0
    }

    fn exports(query: Option<QueryFn>, init: Option<InitFn>) -> Exports {
        Exports {
            query,
            init,
            shutdown: None,
            legacy_init: None,
        }
    }

    fn run(exports: Exports, manifest: Option<&Manifest>) -> Result<String, String> {
        activate(
            Path::new("C:\\plugins\\fake.dll"),
            manifest,
            std::ptr::null_mut(),
            exports,
            &HOST.0,
        )
    }

    #[test]
    fn activate_runs_init_with_the_host_and_returns_the_descriptor_id() {
        let before = INIT_CALLS.load(Ordering::SeqCst);
        let id = run(exports(Some(query_valid), Some(init_ok)), None).unwrap();
        assert_eq!(id, "com.example.fake");
        assert!(INIT_CALLS.load(Ordering::SeqCst) > before);
        assert_eq!(SEEN_HOST_ABI.load(Ordering::SeqCst), ABI_VERSION as usize);
        assert!(loaded().contains(&"com.example.fake".to_string()));
    }

    #[test]
    fn activate_accepts_a_descriptor_from_a_newer_sdk_of_the_same_abi() {
        assert!(run(exports(Some(query_grown), Some(init_ok)), None).is_ok());
    }

    #[test]
    fn activate_falls_back_to_the_path_for_a_nameless_descriptor() {
        let id = run(exports(Some(query_anonymous), Some(init_ok)), None).unwrap();
        assert_eq!(id, "C:\\plugins\\fake.dll");
    }

    #[test]
    fn activate_rejects_a_null_descriptor() {
        let error = run(exports(Some(query_null), Some(init_ok)), None).unwrap_err();
        assert_eq!(error, "query returned no descriptor");
    }

    #[test]
    fn activate_rejects_a_descriptor_that_is_too_small() {
        let error = run(exports(Some(query_small), Some(init_ok)), None).unwrap_err();
        assert!(error.contains("descriptor is"), "{error}");
        assert!(error.contains("expected at least"), "{error}");
    }

    #[test]
    fn activate_rejects_a_plugin_built_for_a_newer_abi() {
        let before = INIT_CALLS.load(Ordering::SeqCst);
        let error = run(exports(Some(query_newer), Some(init_ok)), None).unwrap_err();
        assert!(error.contains("needs ABI"), "{error}");
        assert_eq!(
            INIT_CALLS.load(Ordering::SeqCst),
            before,
            "init must not run for a plugin the loader cannot serve"
        );
    }

    #[test]
    fn activate_requires_init_next_to_the_descriptor() {
        let error = run(exports(Some(query_valid), None), None).unwrap_err();
        assert!(error.contains(EXPORT_INIT), "{error}");
    }

    #[test]
    fn activate_turns_a_failing_init_into_an_error() {
        let error = run(exports(Some(query_valid), Some(init_fails)), None).unwrap_err();
        assert_eq!(error, "init returned 3");
    }

    #[test]
    fn activate_without_any_entry_point_names_both_exports() {
        let error = run(exports(None, None), None).unwrap_err();
        assert!(error.contains(EXPORT_QUERY), "{error}");
        assert!(error.contains(EXPORT_LEGACY_INIT), "{error}");
    }

    #[test]
    fn activate_calls_the_legacy_entry_point_with_a_null_argument() {
        let id = run(
            Exports {
                query: None,
                init: None,
                shutdown: None,
                legacy_init: Some(legacy),
            },
            None,
        )
        .unwrap();
        assert_eq!(id, "C:\\plugins\\fake.dll");
        assert_eq!(LEGACY_ARG_WAS_NULL.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_descriptor_wins_over_the_legacy_export() {
        let before = INIT_CALLS.load(Ordering::SeqCst);
        let id = run(
            Exports {
                query: Some(query_valid),
                init: Some(init_ok),
                shutdown: None,
                legacy_init: Some(legacy),
            },
            None,
        )
        .unwrap();
        assert_eq!(id, "com.example.fake");
        assert!(INIT_CALLS.load(Ordering::SeqCst) > before);
    }

    // ---- load_dll ---------------------------------------------------------

    #[test]
    fn load_dll_reports_a_missing_file_with_both_error_codes() {
        let dir = TempDir::new("load-missing");
        let error = load_dll(&dir.path().join("nope.dll"), None).unwrap_err();
        assert!(error.contains("could not load"), "{error}");
        assert!(error.contains("LoadLibraryW error"), "{error}");
        assert!(error.contains("LoadLibraryExW error"), "{error}");
    }

    #[test]
    fn load_dll_rejects_a_file_that_is_not_a_pe_image() {
        let dir = TempDir::new("load-garbage");
        let path = dir.write("garbage.dll", "this is not a dll");
        assert!(load_dll(&path, None).is_err());
    }

    #[test]
    fn load_dll_rejects_a_dll_without_plugin_exports() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let path = PathBuf::from(root).join("System32").join("kernel32.dll");
        let error = load_dll(&path, None).unwrap_err();
        assert!(error.contains(EXPORT_QUERY), "{error}");
    }

    // ---- scanning ---------------------------------------------------------

    fn config_for(dirs: &[&Path]) -> Config {
        Config {
            log_candidates: Vec::new(),
            plugin_dirs: dirs.iter().map(|dir| dir.to_path_buf()).collect(),
            devtools_port: None,
            any_host: false,
        }
    }

    #[test]
    fn scan_of_a_missing_directory_does_nothing() {
        let dir = TempDir::new("scan-missing");
        let summary = scan(&config_for(&[&dir.path().join("absent")]));
        assert_eq!((summary.loaded, summary.skipped, summary.failed), (0, 0, 0));
    }

    #[test]
    fn scan_of_an_empty_directory_does_nothing() {
        let dir = TempDir::new("scan-empty");
        let summary = scan(&config_for(&[dir.path()]));
        assert_eq!((summary.loaded, summary.skipped, summary.failed), (0, 0, 0));
    }

    #[test]
    fn scan_classifies_every_kind_of_plugin_directory() {
        let dir = TempDir::new("scan-mixed");
        // skipped: js plugin (needs the runtime), then a duplicate of it
        dir.write(
            "a-js/plugin.toml",
            "id = \"js.one\"\nkind = \"js\"\nentry = \"main.js\"\n",
        );
        dir.write("b-dup/plugin.toml", "id = \"js.one\"\nkind = \"js\"\n");
        // skipped: disabled in the manifest
        dir.write("c-disabled/plugin.toml", "id = \"off\"\nenabled = false\n");
        // skipped: a directory without a manifest
        dir.write("d-nomanifest/notes.txt", "hello");
        // failed: manifest that does not parse
        dir.write("e-badtoml/plugin.toml", "id = ");
        // failed: asks for an ABI the loader does not have
        dir.write("f-newapi/plugin.toml", "id = \"new\"\napi = 99\n");
        // failed: unknown kind
        dir.write("g-kind/plugin.toml", "id = \"kind\"\nkind = \"lua\"\n");
        // failed: dll plugin with nothing to load
        dir.write("h-nodll/plugin.toml", "id = \"nodll\"\n");
        // failed: the entry is not a real DLL
        dir.write(
            "i-garbage/plugin.toml",
            "id = \"garbage\"\nentry = \"x.dll\"\n",
        );
        dir.write("i-garbage/x.dll", "not a dll");
        // skipped: same id as a plugin that already claimed it, even though
        // that one failed to load
        dir.write(
            "j-garbage-again/plugin.toml",
            "id = \"garbage\"\nentry = \"x.dll\"\n",
        );
        dir.write("j-garbage-again/x.dll", "not a dll");
        // failed: a loose DLL that is not loadable
        dir.write("loose.dll", "not a dll");
        // ignored: files that are not DLLs
        dir.write("readme.txt", "ignore me");
        dir.write("LOOSE.TXT", "ignore me");

        let summary = scan(&config_for(&[dir.path()]));
        assert_eq!(summary.loaded, 0);
        assert_eq!(
            summary.skipped, 5,
            "js, duplicate js, disabled, no manifest, duplicate of a failed id"
        );
        assert_eq!(
            summary.failed, 6,
            "bad toml, new api, unknown kind, no dll, garbage dll, loose dll"
        );
    }

    #[test]
    fn scan_deduplicates_ids_across_directories() {
        let first = TempDir::new("scan-first");
        let second = TempDir::new("scan-second");
        first.write("p/plugin.toml", "id = \"same\"\nkind = \"js\"\n");
        second.write("p/plugin.toml", "id = \"same\"\nkind = \"js\"\n");
        second.write("q/plugin.toml", "id = \"other\"\nkind = \"js\"\n");

        let summary = scan(&config_for(&[first.path(), second.path()]));
        assert_eq!(summary.skipped, 3);
        assert_eq!(summary.failed, 0);
    }

    #[test]
    fn scan_picks_the_manifest_entry_over_other_dlls() {
        let dir = TempDir::new("scan-entry");
        dir.write(
            "p/plugin.toml",
            "id = \"entry\"\nentry = \"sub/real.dll\"\n",
        );
        dir.write("p/aaa.dll", "decoy");
        // The entry does not exist, so loading fails; if the decoy had been
        // picked the error would name aaa.dll instead.
        let summary = scan(&config_for(&[dir.path()]));
        assert_eq!(summary.failed, 1);
    }

    #[test]
    fn plugin_kinds_are_the_documented_ones() {
        assert_eq!(KIND_DLL, "dll");
        assert_eq!(KIND_JS, "js");
        assert_eq!(MANIFEST, "plugin.toml");
    }
}
