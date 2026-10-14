//! Entry points the macOS e2e suite drives through `dlopen`.
//!
//! Built only with the `harness` feature. They bypass the activation gate on
//! purpose: the gate is part of the bootstrap work and is tested with a real
//! patched copy, not here. Nothing in this module is a product surface.

use std::ffi::{c_char, CStr};
use std::path::{Path, PathBuf};

use crate::Config;

/// Test-only installation of a home: this does not enable vendor execution
/// or bypass the production constructor's isolation refusal.
#[no_mangle]
pub extern "C" fn ParsecOrbitMacosHarnessHome(home: *const c_char) -> i32 {
    let Some(home) = cstr(home) else {
        return -1;
    };
    let Ok(home) = std::ffi::CString::new(home) else {
        return -1;
    };
    match crate::nss::install(unsafe { libc::getuid() }, home) {
        Ok(()) => 0,
        Err(_) => -2,
    }
}

/// Test-only installation of the shared-persistent redirect with owned
/// scratch paths. This does not enable vendor execution or install a
/// production prefix.
#[no_mangle]
pub extern "C" fn ParsecOrbitMacosHarnessRedirect(
    prefix: *const c_char,
    target: *const c_char,
) -> i32 {
    let (Some(prefix), Some(target)) = (cstr(prefix), cstr(target)) else {
        return -1;
    };
    match crate::redirect::install(Path::new(prefix), Path::new(target)) {
        Ok(()) => 0,
        Err(_) => -2,
    }
}

fn cstr(pointer: *const c_char) -> Option<&'static str> {
    if pointer.is_null() {
        return None;
    }
    // Safety: the caller passes NUL-terminated strings that stay alive for
    // the call.
    unsafe { CStr::from_ptr(pointer) }.to_str().ok()
}

/// Runs one scan. `log` and `plugin_dirs` are required NUL-terminated UTF-8
/// strings; `plugin_dirs` is `:`-separated; `data_root` may be null.
///
/// Returns the number of failed plugins, or -1 when an argument is missing.
#[no_mangle]
pub extern "C" fn ParsecOrbitMacosHarnessRun(
    log: *const c_char,
    plugin_dirs: *const c_char,
    data_root: *const c_char,
) -> i32 {
    let (Some(log), Some(plugin_dirs)) = (cstr(log), cstr(plugin_dirs)) else {
        return -1;
    };
    let data_root = cstr(data_root).map(PathBuf::from);
    let config = Config {
        orbit_root: data_root.clone().unwrap_or_default(),
        log_candidates: vec![PathBuf::from(log)],
        plugin_dirs: plugin_dirs
            .split(':')
            .map(|part| PathBuf::from(part.trim()))
            .collect(),
        data_root,
        staging: None,
    };
    let _ = crate::log::init(&config.log_candidates);
    crate::scan(&config).failed as i32
}

/// Whether an interface with this name is registered in this process.
/// Returns 1 or 0, or -1 when the argument is missing.
#[no_mangle]
pub extern "C" fn ParsecOrbitMacosHarnessFind(name: *const c_char) -> i32 {
    let Some(name) = cstr(name) else {
        return -1;
    };
    i32::from(!parsec_orbit_core::host::find(name).is_null())
}
