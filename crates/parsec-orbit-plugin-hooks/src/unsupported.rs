//! Explicit fail-closed hooks service for targets without a backend.

use std::ffi::c_void;
use std::mem::size_of;

use parsec_orbit_plugin_sdk_std::prelude::*;
use parsec_orbit_plugin_sdk_std::services::Hooks;
use parsec_orbit_plugin_sdk_std::{ParsecOrbitHooksV1, ParsecOrbitInterfaceHeader, ERR_NOT_READY};

unsafe extern "system" fn resolve(
    _module: *const u8,
    _module_len: usize,
    _symbol: *const u8,
    _symbol_len: usize,
) -> *const c_void {
    std::ptr::null()
}

unsafe extern "system" fn install(
    _target: *const c_void,
    _detour: *const c_void,
    _original: *mut *const c_void,
) -> i64 {
    ERR_NOT_READY as i64
}

unsafe extern "system" fn unavailable(_id: i64) -> i32 {
    ERR_NOT_READY
}

/// The table registered under parsec-orbit.hooks.
pub static HOOKS: ParsecOrbitHooksV1 = ParsecOrbitHooksV1 {
    header: ParsecOrbitInterfaceHeader::new(size_of::<ParsecOrbitHooksV1>(), 1, 0),
    resolve: Some(resolve),
    install: Some(install),
    enable: Some(unavailable),
    disable: Some(unavailable),
    remove: Some(unavailable),
};

#[derive(Default)]
struct HooksPlugin;

impl Plugin for HooksPlugin {
    fn init(&mut self, cx: &Context) -> Result {
        info!(cx, "hooks backend is unavailable on this target");
        cx.register::<Hooks>(&HOOKS)?;
        Ok(())
    }
}

parsec_orbit_plugin_sdk_std::declare_plugin! {
    id = "com.parsec-orbit.hooks",
    name = "Hooks",
    version = "0.1.0",
    author = "Parsec Orbit contributors",
    description = "Provides parsec-orbit.hooks when a platform backend is available",
    license = "MIT",
    plugin = HooksPlugin,
}
