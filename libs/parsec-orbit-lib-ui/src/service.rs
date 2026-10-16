//! The static service table and extern "system" entry points for `parsec-orbit.ui`.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use parsec_orbit_plugin_sdk::{
    MessageCallback, ParsecOrbitInterfaceHeader, ParsecOrbitUiV2, ERR_INVALID, OK,
};

use crate::companion::Log;
use crate::registry::{Failure, Registry};

struct State {
    registry: Arc<Registry>,
    visible: Arc<AtomicBool>,
    companion: OnceLock<crate::companion::Companion>,
    log: OnceLock<Log>,
}

// Reuses the ABI service's existing process-lifetime registry owner. The GUI
// lives in its own process; no GUI objects or callback pointers cross the pipe.
static REGISTRY: OnceLock<State> = OnceLock::new();

fn state() -> &'static State {
    REGISTRY.get_or_init(|| State {
        registry: Arc::new(Registry::new()),
        visible: Arc::new(AtomicBool::new(false)),
        companion: OnceLock::new(),
        log: OnceLock::new(),
    })
}

pub fn registry() -> &'static Registry {
    &state().registry
}

/// Starts the bundled renderer after the ABI service has been registered.
#[cfg(any(windows, target_os = "macos"))]
pub fn start(cx: parsec_orbit_plugin_sdk::Context) -> std::io::Result<()> {
    use parsec_orbit_plugin_sdk::prelude::*;
    use std::path::Path;
    let directory = cx.plugin_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "UI plugin directory is unavailable",
        )
    })?;
    let executable = Path::new(directory).join(if cfg!(windows) {
        "parsec-orbit-ui-companion.exe"
    } else {
        "parsec-orbit-ui-companion"
    });
    let owner = state();
    let log: Log = Arc::new(move |message| warn!(cx, "orbit ui: {}", message));
    let _ = owner.log.set(log.clone());
    if owner.companion.get().is_none() {
        let companion = crate::companion::Companion::start(
            executable,
            owner.registry.clone(),
            owner.visible.clone(),
            log,
        )?;
        let _ = owner.companion.set(companion);
    }
    Ok(())
}

unsafe fn utf8<'a>(pointer: *const u8, len: usize) -> Option<&'a str> {
    if pointer.is_null() {
        return if len == 0 { Some("") } else { None };
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(pointer, len) }).ok()
}

pub fn toggle() {
    if let Some(companion) = state().companion.get() {
        companion.toggle();
    } else {
        state().visible.fetch_xor(true, Ordering::SeqCst);
    }
}

fn pages_changed() {
    if let Some(companion) = state().companion.get() {
        companion.refresh();
    }
}

/// Reports a refused request through the plugin logger and returns its ABI code.
///
/// The ABI carries only the code, so the reason is how a plugin author learns
/// which rule a page or patch broke.
fn refused(what: &str, failure: Failure) -> i32 {
    if let Some(log) = state().log.get() {
        log(&format!("{what} refused: {}", failure.reason));
    }
    failure.code
}

/// Registers a page. Only the service's own pages pass `reserved`.
pub fn register_page(page_json: &str, reserved: bool) -> i32 {
    match registry().register_page(page_json, reserved) {
        Ok(()) => {
            pages_changed();
            OK
        }
        Err(failure) => refused("page registration", failure),
    }
}

/// Unregisters a page.
pub fn unregister_page(page_id: &str) -> i32 {
    let result = registry().unregister_page(page_id);
    if result == OK {
        pages_changed();
    }
    result
}

/// Applies a patch to a page.
pub fn update_page(page_id: &str, patch_json: &str) -> i32 {
    match registry().update_page(page_id, patch_json) {
        Ok(()) => {
            pages_changed();
            OK
        }
        Err(failure) => refused(&format!("update of page {page_id:?}"), failure),
    }
}

/// Selects a page for the window to show.
pub fn navigate(page_id: &str) -> i32 {
    let result = registry().navigate(page_id);
    if result == OK {
        if let Some(companion) = state().companion.get() {
            companion.navigate();
        }
    }
    result
}

unsafe extern "system" fn svc_show() -> i32 {
    if let Some(companion) = state().companion.get() {
        companion.set_visible(true);
    } else {
        state().visible.store(true, Ordering::SeqCst);
    }
    0
}

unsafe extern "system" fn svc_hide() -> i32 {
    if let Some(companion) = state().companion.get() {
        companion.set_visible(false);
    } else {
        state().visible.store(false, Ordering::SeqCst);
    }
    0
}

unsafe extern "system" fn svc_is_visible() -> i32 {
    i32::from(state().visible.load(Ordering::SeqCst))
}

unsafe extern "system" fn svc_navigate(id: *const u8, len: usize) -> i32 {
    match unsafe { utf8(id, len) } {
        Some(id) => navigate(id),
        None => ERR_INVALID,
    }
}

unsafe extern "system" fn svc_register_page(page: *const u8, len: usize) -> i32 {
    match unsafe { utf8(page, len) } {
        Some(page) => register_page(page, false),
        None => ERR_INVALID,
    }
}

unsafe extern "system" fn svc_unregister_page(id: *const u8, len: usize) -> i32 {
    match unsafe { utf8(id, len) } {
        Some(id) => unregister_page(id),
        None => ERR_INVALID,
    }
}

unsafe extern "system" fn svc_update_page(
    id: *const u8,
    id_len: usize,
    patch: *const u8,
    patch_len: usize,
) -> i32 {
    match (unsafe { utf8(id, id_len) }, unsafe {
        utf8(patch, patch_len)
    }) {
        (Some(id), Some(patch)) => update_page(id, patch),
        _ => ERR_INVALID,
    }
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
pub static UI_SERVICE: ParsecOrbitUiV2 = ParsecOrbitUiV2 {
    header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitUiV2>(), 2, 0),
    show: Some(svc_show),
    hide: Some(svc_hide),
    is_visible: Some(svc_is_visible),
    navigate: Some(svc_navigate),
    register_page: Some(svc_register_page),
    unregister_page: Some(svc_unregister_page),
    update_page: Some(svc_update_page),
    on_action: Some(svc_on_action),
    off_action: Some(svc_off_action),
};
