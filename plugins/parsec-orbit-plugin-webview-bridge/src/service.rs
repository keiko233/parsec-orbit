//! The `parsec-orbit.webview` interface over the process-wide [`Bridge`].

use std::ffi::c_void;
use std::sync::OnceLock;

use parsec_orbit_plugin_sdk_std::{
    MessageCallback, ParsecOrbitInterfaceHeader, ParsecOrbitWebViewV1, ERR_INVALID, ERR_NOT_READY,
    OK,
};

use crate::bridge::Bridge;

static BRIDGE: OnceLock<Bridge> = OnceLock::new();

/// The bridge the interface functions talk to.
pub fn bridge() -> &'static Bridge {
    BRIDGE.get_or_init(Bridge::new)
}

/// The bridge, with its session started: the first call that needs the page
/// connects to it.
fn busy() -> &'static Bridge {
    let bridge = bridge();
    bridge.start_if_configured();
    bridge
}

/// A UTF-8 argument; `None` for a null pointer or invalid UTF-8.
unsafe fn utf8<'a>(pointer: *const u8, len: usize) -> Option<&'a str> {
    if pointer.is_null() {
        return if len == 0 { Some("") } else { None };
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(pointer, len) }).ok()
}

unsafe extern "system" fn is_ready() -> i32 {
    bridge().is_ready() as i32
}

unsafe extern "system" fn add_script(
    key: *const u8,
    key_len: usize,
    script: *const u8,
    script_len: usize,
) -> i64 {
    let (Some(key), Some(script)) = (unsafe { utf8(key, key_len) }, unsafe {
        utf8(script, script_len)
    }) else {
        return ERR_INVALID as i64;
    };
    if key.is_empty() || script.is_empty() {
        return ERR_INVALID as i64;
    }
    busy().add_script(key, script)
}

unsafe extern "system" fn remove_script(script_id: i64) -> i32 {
    bridge().remove_script(script_id)
}

unsafe extern "system" fn evaluate(
    expression: *const u8,
    len: usize,
    out: Option<MessageCallback>,
    user: *mut c_void,
) -> i32 {
    let Some(expression) = (unsafe { utf8(expression, len) }) else {
        return ERR_INVALID;
    };
    match busy().evaluate(expression) {
        Ok(json) => {
            if let Some(out) = out {
                unsafe { out(user, json.as_ptr(), json.len()) };
            }
            OK
        }
        Err(code) => code,
    }
}

unsafe extern "system" fn subscribe(
    channel: *const u8,
    channel_len: usize,
    callback: Option<MessageCallback>,
    user: *mut c_void,
) -> i64 {
    let (Some(channel), Some(callback)) = (unsafe { utf8(channel, channel_len) }, callback) else {
        return ERR_INVALID as i64;
    };
    busy().subscribe(channel, callback, user)
}

unsafe extern "system" fn unsubscribe(subscription: i64) -> i32 {
    bridge().unsubscribe(subscription)
}

unsafe extern "system" fn post(
    channel: *const u8,
    channel_len: usize,
    payload: *const u8,
    len: usize,
) -> i32 {
    let (Some(channel), Some(payload)) = (unsafe { utf8(channel, channel_len) }, unsafe {
        utf8(payload, len)
    }) else {
        return ERR_INVALID;
    };
    busy().post(channel, payload)
}

/// The table registered under `parsec-orbit.webview`.
pub static WEBVIEW: ParsecOrbitWebViewV1 = ParsecOrbitWebViewV1 {
    header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitWebViewV1>(), 1, 0),
    is_ready: Some(is_ready),
    add_script: Some(add_script),
    remove_script: Some(remove_script),
    evaluate: Some(evaluate),
    subscribe: Some(subscribe),
    unsubscribe: Some(unsubscribe),
    post: Some(post),
};

/// What an unusable call reports when there is no bridge session at all.
#[allow(dead_code)]
pub const NO_SESSION: i32 = ERR_NOT_READY;
