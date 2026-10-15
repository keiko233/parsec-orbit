//! The static service table and extern "system" entry points for `parsec-orbit.ui`.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use parsec_orbit_plugin_sdk::{
    MessageCallback, ParsecOrbitInterfaceHeader, ParsecOrbitUiV1, ERR_INVALID, OK,
};

use crate::registry::Registry;

struct State {
    registry: Arc<Registry>,
    visible: Arc<AtomicBool>,
    companion: OnceLock<crate::companion::Companion>,
}

// Reuses the ABI service's existing process-lifetime registry owner. The GUI
// lives in its own process; no GUI objects or callback pointers cross the pipe.
static REGISTRY: OnceLock<State> = OnceLock::new();

fn state() -> &'static State {
    REGISTRY.get_or_init(|| State {
        registry: Arc::new(Registry::new()),
        visible: Arc::new(AtomicBool::new(false)),
        companion: OnceLock::new(),
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
    if owner.companion.get().is_none() {
        let companion = crate::companion::Companion::start(
            executable,
            owner.registry.clone(),
            owner.visible.clone(),
            Arc::new(move |message| warn!(cx, "orbit ui: {}", message)),
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

fn sections_changed() {
    if let Some(companion) = state().companion.get() {
        companion.refresh();
    }
}

fn state_updated(_section: &str) {
    sections_changed();
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

unsafe extern "system" fn svc_register_section(schema: *const u8, len: usize) -> i32 {
    let Some(schema_str) = (unsafe { utf8(schema, len) }) else {
        return ERR_INVALID;
    };
    let result = registry().register_section(schema_str);
    if result == OK {
        sections_changed();
    }
    result
}

unsafe extern "system" fn svc_unregister_section(id: *const u8, len: usize) -> i32 {
    let Some(id_str) = (unsafe { utf8(id, len) }) else {
        return ERR_INVALID;
    };
    let result = registry().unregister_section(id_str);
    if result == OK {
        sections_changed();
    }
    result
}

unsafe extern "system" fn svc_update_state(
    id: *const u8,
    id_len: usize,
    state: *const u8,
    state_len: usize,
) -> i32 {
    let (Some(id_str), Some(state_str)) = (unsafe { utf8(id, id_len) }, unsafe {
        utf8(state, state_len)
    }) else {
        return ERR_INVALID;
    };
    let result = registry().update_state(id_str, state_str);
    if result == OK {
        state_updated(id_str);
    }
    result
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
    header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitUiV1>(), 1, 0),
    show: Some(svc_show),
    hide: Some(svc_hide),
    is_visible: Some(svc_is_visible),
    register_section: Some(svc_register_section),
    unregister_section: Some(svc_unregister_section),
    update_state: Some(svc_update_state),
    on_action: Some(svc_on_action),
    off_action: Some(svc_off_action),
};
