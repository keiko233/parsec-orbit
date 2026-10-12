//! The two interfaces the runtime registers: `parsec-orbit.kind.js` and
//! `parsec-orbit.js`.

use std::ffi::{c_void, CStr};
use std::path::PathBuf;

use parsec_orbit_plugin_sdk_std::{
    services::kind_handler, MessageCallback, ParsecOrbitInterfaceHeader, ParsecOrbitJsV1,
    ParsecOrbitKindHandlerV1, ParsecOrbitPluginLoadInfo, ERR_EXISTS, ERR_INVALID, ERR_NOT_FOUND,
    OK,
};

use crate::loader::{self, LoadRequest};

unsafe fn c_text(pointer: *const u8) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(pointer as *const _) }
        .to_str()
        .ok()
        .map(str::to_string)
}

unsafe fn text<'a>(pointer: *const u8, len: usize) -> Option<&'a str> {
    if pointer.is_null() {
        return None;
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(pointer, len) }).ok()
}

// ---- parsec-orbit.kind.js -------------------------------------------------------------

unsafe extern "system" fn load_plugin(info: *const ParsecOrbitPluginLoadInfo) -> i32 {
    let Some(info) = (unsafe { info.as_ref() }) else {
        return ERR_INVALID;
    };
    let (Some(id), Some(dir), Some(entry)) = (
        unsafe { c_text(info.id) },
        unsafe { c_text(info.dir) },
        unsafe { c_text(info.entry) },
    ) else {
        return ERR_INVALID;
    };
    let manifest = unsafe { text(info.manifest, info.manifest_len) }
        .unwrap_or_default()
        .to_string();
    let request = LoadRequest {
        id: id.clone(),
        dir: PathBuf::from(dir),
        entry: PathBuf::from(entry),
        data_dir: unsafe { c_text(info.data_dir) }.map(PathBuf::from),
        manifest,
    };
    match loader::load_plugin(request) {
        Ok(()) => OK,
        Err(error) => {
            loader::report(&id, &error);
            1
        }
    }
}

/// `parsec-orbit.kind.js`, version 1.0.
pub static KIND_HANDLER: ParsecOrbitKindHandlerV1 = kind_handler(load_plugin, None);

// ---- parsec-orbit.js --------------------------------------------------------------------

unsafe extern "system" fn create_context(name: *const u8, len: usize) -> i64 {
    let Some(name) = (unsafe { text(name, len) }) else {
        return ERR_INVALID as i64;
    };
    if name.is_empty() {
        return ERR_INVALID as i64;
    }
    match loader::create_context(name) {
        Ok(id) => id,
        Err(code) => code as i64,
    }
}

unsafe extern "system" fn eval(
    context: i64,
    source: *const u8,
    len: usize,
    out: Option<MessageCallback>,
    user: *mut c_void,
) -> i32 {
    let Some(source) = (unsafe { text(source, len) }) else {
        return ERR_INVALID;
    };
    let Some(handle) = loader::native_context(context) else {
        return ERR_NOT_FOUND;
    };
    match handle.eval(source) {
        Ok(json) => {
            if let Some(out) = out {
                unsafe { out(user, json.as_ptr(), json.len()) };
            }
            OK
        }
        Err(error) => {
            loader::report_eval(context, &error.message);
            error.code
        }
    }
}

unsafe extern "system" fn destroy_context(context: i64) -> i32 {
    if loader::destroy_context(context) {
        OK
    } else {
        ERR_NOT_FOUND
    }
}

/// `parsec-orbit.js`, version 1.0.
pub static JS: ParsecOrbitJsV1 = ParsecOrbitJsV1 {
    header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitJsV1>(), 1, 0),
    create_context: Some(create_context),
    eval: Some(eval),
    destroy_context: Some(destroy_context),
};

#[allow(dead_code)]
const _: i32 = ERR_EXISTS;
