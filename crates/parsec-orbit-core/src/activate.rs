//! Validation of a mapped plugin and the call into its entry points.

use std::ffi::CStr;
use std::path::Path;

use parsec_orbit_plugin_abi::{
    ParsecOrbitHost, ParsecOrbitPluginDesc, ABI_VERSION, EXPORT_INIT, EXPORT_LEGACY_INIT,
    EXPORT_QUERY,
};

use crate::host::{data_dir_for, host_for};
use crate::manifest::describe;
use crate::{Env, Exports, Loaded, Manifest};

/// The plugin a mapped library belongs to.
#[derive(Clone, Copy)]
pub struct Plugin<'a> {
    /// Plugin directory, the base of everything relative.
    pub dir: &'a Path,
    /// The library that was mapped.
    pub entry: &'a Path,
    /// `None` for a loose DLL.
    pub manifest: Option<&'a Manifest>,
}

unsafe fn text(pointer: *const u8) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    // Safety: the plugin descriptor contract requires NUL-terminated UTF-8
    // strings that stay valid for the lifetime of the module.
    unsafe { CStr::from_ptr(pointer as *const _) }
        .to_str()
        .ok()
        .map(str::to_string)
}

/// Names of the fields the manifest sets to something other than the
/// descriptor's value.
fn mismatches(manifest: &Manifest, descriptor: [(&'static str, &str); 6]) -> Vec<&'static str> {
    let declared = [
        manifest.id.as_deref(),
        manifest.name.as_deref(),
        manifest.version.as_deref(),
        manifest.author.as_deref(),
        manifest.description.as_deref(),
        manifest.license.as_deref(),
    ];
    descriptor
        .iter()
        .zip(declared)
        .filter_map(|((field, actual), declared)| match declared {
            Some(declared) if declared != *actual => Some(*field),
            _ => None,
        })
        .collect()
}

/// Validates the descriptor of a mapped plugin and runs its entry points.
///
/// A plugin whose `init` ran is pushed onto `registry` even when `init`
/// reports failure: the module stays mapped, so it stays on record.
///
/// The plugin is identified by its manifest id, then by the descriptor id,
/// then by the path of the library. The id is what its host table carries and
/// what dependents wait for. Returns it on success.
pub fn activate(
    env: Env,
    plugin: Plugin,
    module: usize,
    exports: Exports,
    registry: &mut Vec<Loaded>,
) -> Result<String, String> {
    let Plugin {
        dir,
        entry: path,
        manifest,
    } = plugin;
    let label = describe(manifest, path);
    let Some(query) = exports.query else {
        // Plugin from before the descriptor existed: single legacy entry point.
        let Some(legacy) = exports.legacy_init else {
            return Err(format!("no {EXPORT_QUERY} or {EXPORT_LEGACY_INIT} export"));
        };
        let result = unsafe { legacy(std::ptr::null_mut()) };
        env.logger.warn(&format!(
            "{label}: legacy entry point, {EXPORT_LEGACY_INIT} returned {result}"
        ));
        let id = path.display().to_string();
        registry.push(Loaded {
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
    if (descriptor as usize) % std::mem::align_of::<ParsecOrbitPluginDesc>() != 0 {
        return Err("query returned a misaligned descriptor".to_string());
    }
    // Safety: the query export must return a readable descriptor of at least
    // `size_of::<ParsecOrbitPluginDesc>()` bytes; the following size check
    // rejects a smaller `struct_size` before any tail fields are read.
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

    let descriptor_id = unsafe { text(descriptor.id) };
    let id = manifest
        .and_then(|manifest| manifest.id.clone())
        .or_else(|| descriptor_id.clone())
        .unwrap_or_else(|| path.display().to_string());
    let version = unsafe { text(descriptor.version) }.unwrap_or_else(|| "-".to_string());
    let display = unsafe { text(descriptor.name) }.unwrap_or_default();
    let author = unsafe { text(descriptor.author) }.unwrap_or_default();
    let description = unsafe { text(descriptor.description) }.unwrap_or_default();
    let license = unsafe { text(descriptor.license) }.unwrap_or_default();

    let Some(init) = exports.init else {
        return Err(format!("descriptor found but {EXPORT_INIT} is missing"));
    };

    if let Some(manifest) = manifest {
        let mismatches = mismatches(
            manifest,
            [
                ("id", descriptor_id.as_deref().unwrap_or("")),
                ("name", &display),
                ("version", &version),
                ("author", &author),
                ("description", &description),
                ("license", &license),
            ],
        );
        if !mismatches.is_empty() {
            env.logger.warn(&format!(
                "{label}: manifest and descriptor disagree on {} (the descriptor is shown)",
                mismatches.join(", ")
            ));
        }
    }

    let absolute = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let data_dir = env.data_root.map(|root| data_dir_for(root, &id));
    let host = host_for(env.loader_version, &id, &absolute, data_dir.as_deref());
    let result = unsafe { init(host as *const ParsecOrbitHost) };

    env.logger.info(&format!(
        "loaded {id} {version} ({display}) ABI {} by {author} [{license}] - {description} - init returned {result}",
        descriptor.abi_version
    ));

    registry.push(Loaded {
        id: id.clone(),
        module,
        shutdown: exports.shutdown,
    });

    if result != 0 {
        return Err(format!("init returned {result}"));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Logger, Module, Platform};
    use parsec_orbit_plugin_abi::{InitFn, QueryFn};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Quiet;
    impl Logger for Quiet {
        fn log(&self, _level: crate::Level, _message: &str) {}
    }

    struct NoPlatform;
    impl Platform for NoPlatform {
        fn load(&self, _path: &Path) -> Result<Module, String> {
            Err("not used".into())
        }
    }

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

    unsafe extern "system" fn query_misaligned(_abi: u32) -> *const ParsecOrbitPluginDesc {
        #[repr(C, align(8))]
        struct Aligned([u8; 64]);
        static BYTES: Aligned = Aligned([0; 64]);
        (BYTES.0.as_ptr() as usize + 1) as *const ParsecOrbitPluginDesc
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

    fn run_into(
        registry: &mut Vec<Loaded>,
        exports: Exports,
        manifest: Option<&Manifest>,
    ) -> Result<String, String> {
        activate(
            Env {
                platform: &NoPlatform,
                logger: &Quiet,
                loader_version: "test",
                data_root: Some(Path::new("data")),
            },
            Plugin {
                dir: Path::new("C:\\plugins"),
                entry: Path::new("C:\\plugins\\fake.dll"),
                manifest,
            },
            0,
            exports,
            registry,
        )
    }

    fn run(exports: Exports, manifest: Option<&Manifest>) -> Result<String, String> {
        run_into(&mut Vec::new(), exports, manifest)
    }

    #[test]
    fn activate_runs_init_with_the_host_and_returns_the_descriptor_id() {
        let id = run(exports(Some(query_valid), Some(init_ok)), None).unwrap();
        assert_eq!(id, "com.example.fake");
        assert_eq!(SEEN_HOST_ABI.load(Ordering::SeqCst), ABI_VERSION as usize);
        let mut registry = Vec::new();
        run_into(
            &mut registry,
            exports(Some(query_valid), Some(init_ok)),
            None,
        )
        .unwrap();
        assert_eq!(registry[0].id, "com.example.fake");
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
    fn activate_rejects_a_misaligned_descriptor() {
        let error = run(exports(Some(query_misaligned), Some(init_ok)), None).unwrap_err();
        assert_eq!(error, "query returned a misaligned descriptor");
    }

    #[test]
    fn activate_rejects_a_descriptor_that_is_too_small() {
        let error = run(exports(Some(query_small), Some(init_ok)), None).unwrap_err();
        assert!(error.contains("descriptor is"), "{error}");
        assert!(error.contains("expected at least"), "{error}");
    }

    #[test]
    fn activate_rejects_a_plugin_built_for_a_newer_abi() {
        // Its own init and counter: the shared one is bumped by tests running
        // in parallel.
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "system" fn init_counting(_host: *const ParsecOrbitHost) -> i32 {
            CALLS.fetch_add(1, Ordering::SeqCst);
            0
        }
        let error = run(exports(Some(query_newer), Some(init_counting)), None).unwrap_err();
        assert!(error.contains("needs ABI"), "{error}");
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            0,
            "init must not run for a plugin the loader cannot serve"
        );
    }

    #[test]
    fn activate_requires_init_next_to_the_descriptor() {
        let error = run(exports(Some(query_valid), None), None).unwrap_err();
        assert!(error.contains(EXPORT_INIT), "{error}");
    }

    #[test]
    fn activate_turns_a_failing_init_into_an_error_but_keeps_the_module_on_record() {
        let mut registry = Vec::new();
        let error = run_into(
            &mut registry,
            exports(Some(query_valid), Some(init_fails)),
            None,
        )
        .unwrap_err();
        assert_eq!(error, "init returned 3");
        assert_eq!(registry.len(), 1, "the module stays mapped");
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
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "system" fn init_counting(_host: *const ParsecOrbitHost) -> i32 {
            CALLS.fetch_add(1, Ordering::SeqCst);
            0
        }
        let id = run(
            Exports {
                query: Some(query_valid),
                init: Some(init_counting),
                shutdown: None,
                legacy_init: Some(legacy),
            },
            None,
        )
        .unwrap();
        assert_eq!(id, "com.example.fake");
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    }
}
