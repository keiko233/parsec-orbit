//! The static service table and extern "system" entry points for `parsec-orbit.ui`.

use std::ffi::c_void;
use std::sync::OnceLock;

use parsec_orbit_plugin_sdk_std::{
    MessageCallback, ParsecOrbitInterfaceHeader, ParsecOrbitUiV1, ERR_INVALID,
};

use crate::registry::Registry;

static REGISTRY: OnceLock<Registry> = OnceLock::new();

pub fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::new)
}

unsafe fn utf8<'a>(pointer: *const u8, len: usize) -> Option<&'a str> {
    if pointer.is_null() {
        return if len == 0 { Some("") } else { None };
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(pointer, len) }).ok()
}

pub fn toggle() {
    #[cfg(windows)]
    crate::window::toggle();
}

unsafe extern "system" fn svc_show() -> i32 {
    #[cfg(windows)]
    crate::window::show();
    0
}

unsafe extern "system" fn svc_hide() -> i32 {
    #[cfg(windows)]
    crate::window::hide();
    0
}

unsafe extern "system" fn svc_is_visible() -> i32 {
    #[cfg(windows)]
    if crate::window::is_visible() {
        1
    } else {
        0
    }
    #[cfg(not(windows))]
    0
}

unsafe extern "system" fn svc_register_section(schema: *const u8, len: usize) -> i32 {
    let Some(schema_str) = (unsafe { utf8(schema, len) }) else {
        return ERR_INVALID;
    };
    registry().register_section(schema_str)
}

unsafe extern "system" fn svc_unregister_section(id: *const u8, len: usize) -> i32 {
    let Some(id_str) = (unsafe { utf8(id, len) }) else {
        return ERR_INVALID;
    };
    registry().unregister_section(id_str)
}

unsafe extern "system" fn svc_update_state(
    id: *const u8,
    id_len: usize,
    state: *const u8,
    state_len: usize,
) -> i32 {
    let (Some(id_str), Some(state_str)) = (
        unsafe { utf8(id, id_len) },
        unsafe { utf8(state, state_len) },
    ) else {
        return ERR_INVALID;
    };
    registry().update_state(id_str, state_str)
}

unsafe extern "system" fn svc_on_action(
    id: *const u8,
    id_len: usize,
    cb: Option<MessageCallback>,
    user: *mut c_void,
) -> i64 {
    let Some(id_str) = (unsafe { utf8(id, id_len) }) else {
        return ERR_INVALID as i64;
    };
    let Some(cb) = cb else {
        return ERR_INVALID as i64;
    };
    registry().on_action(id_str, cb, user)
}

unsafe extern "system" fn svc_off_action(subscription: i64) -> i32 {
    registry().off_action(subscription)
}

/// The table registered under `parsec-orbit.ui`.
pub static UI_SERVICE: ParsecOrbitUiV1 = ParsecOrbitUiV1 {
    header: ParsecOrbitInterfaceHeader::new(
        std::mem::size_of::<ParsecOrbitUiV1>(),
        1,
        0,
    ),
    show: Some(svc_show),
    hide: Some(svc_hide),
    is_visible: Some(svc_is_visible),
    register_section: Some(svc_register_section),
    unregister_section: Some(svc_unregister_section),
    update_state: Some(svc_update_state),
    on_action: Some(svc_on_action),
    off_action: Some(svc_off_action),
};
