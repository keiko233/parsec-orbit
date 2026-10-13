//! `dlopen`/`dlsym` based [`Platform`].

use std::ffi::{c_void, CStr, CString};
use std::path::Path;

use parsec_orbit_core::{Exports, Module, Platform, KIND_DYLIB};
use parsec_orbit_plugin_abi::{
    InitFn, LegacyInitFn, QueryFn, ShutdownFn, EXPORT_INIT, EXPORT_LEGACY_INIT, EXPORT_QUERY,
    EXPORT_SHUTDOWN,
};

/// Maps plugin dylibs the way `docs/spec/macos-platform.md` specifies:
/// `RTLD_NOW | RTLD_LOCAL`, resolve the four exports with `dlsym`, never
/// `RTLD_GLOBAL`, never `dlclose`.
pub struct MacPlatform;

// The macOS values (`dyld` headers): RTLD_LAZY 0x1, RTLD_NOW 0x2,
// RTLD_LOCAL 0x4, RTLD_GLOBAL 0x8.
const RTLD_NOW: libc::c_int = 0x2;
const RTLD_LOCAL: libc::c_int = 0x4;

unsafe extern "C" {
    fn dlopen(path: *const libc::c_char, mode: libc::c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const libc::c_char) -> *mut c_void;
    fn dlerror() -> *const libc::c_char;
}

/// The pending `dlerror` text, if there is one.
fn last_error() -> String {
    let error = unsafe { dlerror() };
    if error.is_null() {
        return "no dlerror text".to_string();
    }
    unsafe { CStr::from_ptr(error) }
        .to_string_lossy()
        .into_owned()
}

fn c_path(path: &Path) -> Result<CString, String> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("{} contains a NUL byte", path.display()))
}

impl Platform for MacPlatform {
    fn load(&self, path: &Path) -> Result<Module, String> {
        let name = c_path(path)?;
        // A leftover error from an earlier call would otherwise be reported
        // for this one.
        let _ = last_error();
        let handle = unsafe { dlopen(name.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        if handle.is_null() {
            return Err(format!(
                "could not load {} ({})",
                path.display(),
                last_error()
            ));
        }

        let symbol = |symbol: &str| {
            // The correct symbol name is deliberately not a `CString` literal
            // so a typo is a test failure, not a silent null.
            let name = CString::new(symbol).expect("export names hold no NUL");
            unsafe { dlsym(handle, name.as_ptr()) }
        };

        // Safety: the exports are looked up by the names the ABI reserves for
        // these signatures; an image that does not provide them yields null
        // and that entry stays `None`.
        let exports = unsafe {
            let resolved = |pointer: *mut c_void| (!pointer.is_null()).then_some(pointer);
            Exports {
                query: resolved(symbol(EXPORT_QUERY)).map(|f| std::mem::transmute::<_, QueryFn>(f)),
                init: resolved(symbol(EXPORT_INIT)).map(|f| std::mem::transmute::<_, InitFn>(f)),
                shutdown: resolved(symbol(EXPORT_SHUTDOWN))
                    .map(|f| std::mem::transmute::<_, ShutdownFn>(f)),
                legacy_init: resolved(symbol(EXPORT_LEGACY_INIT))
                    .map(|f| std::mem::transmute::<_, LegacyInitFn>(f)),
            }
        };
        Ok(Module {
            handle: handle as usize,
            exports,
        })
    }

    fn native_kind(&self) -> &'static str {
        KIND_DYLIB
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_a_missing_file_reports_the_dlerror_text() {
        let error = MacPlatform
            .load(Path::new("/no/such/libparsec_orbit_test.dylib"))
            .err()
            .expect("a missing file cannot load");
        assert!(error.contains("could not load"), "{error}");
        assert!(
            error.contains("no such file") || error.contains("No such file"),
            "the dlerror text is part of the message: {error}"
        );
    }

    #[test]
    fn loading_a_file_that_is_not_a_mach_o_image_fails() {
        let path = std::env::temp_dir().join(format!(
            "parsec-orbit-not-an-image-{}.dylib",
            std::process::id()
        ));
        std::fs::write(&path, b"this is not a Mach-O image").unwrap();
        let error = MacPlatform.load(&path).err().expect("not an image");
        let _ = std::fs::remove_file(&path);
        assert!(error.contains("could not load"), "{error}");
    }

    #[test]
    fn a_system_library_loads_without_plugin_exports() {
        let module = MacPlatform
            .load(Path::new("/usr/lib/libSystem.B.dylib"))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_ne!(module.handle, 0);
        assert!(module.exports.query.is_none());
        assert!(module.exports.init.is_none());
        assert!(module.exports.shutdown.is_none());
        assert!(module.exports.legacy_init.is_none());
    }

    #[test]
    fn the_native_kind_is_dylib() {
        assert_eq!(MacPlatform.native_kind(), "dylib");
    }
}
