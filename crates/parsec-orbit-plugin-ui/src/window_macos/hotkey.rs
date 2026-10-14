//! The companion window's external activation entry points on macOS.
//!
//! Two global hotkeys toggle the companion window, and a distributed
//! notification listener allows external tools (such as CLI scripts) to toggle
//! the window across process boundaries.
//!
//! # Why Carbon Global Hotkeys?
//!
//! On macOS, global hotkeys can be implemented in two ways:
//! 1. `NSEvent.addGlobalMonitorForEventsMatchingMask` or `CGEventTapCreate`:
//!    These APIs require macOS Accessibility permissions (TCC / `AXIsProcessTrusted`).
//!    Without user authorization in System Settings, event taps fail or silently drop events.
//!    Additionally, global monitors observe keystrokes passively without consuming them.
//! 2. Carbon HIToolbox (`RegisterEventHotKey`):
//!    Carbon's hotkey API registers shortcuts directly with WindowServer and HIToolbox.
//!    It requires zero Accessibility / TCC permissions, functioning reliably in unprivileged
//!    applications. When the application event loop (`[NSApp run]`) runs on the main thread,
//!    HIToolbox attaches an event source to `kCFRunLoopCommonModes`, delivering
//!    `kEventHotKeyPressed` without requiring manual event pumping or interfering with AppKit.
//!
//! # Why CFDistributedNotificationCenter?
//!
//! On Windows, external processes toggle the window by posting a registered window message
//! to a hidden activation window. macOS has no HWNDs or system-wide broadcast window messages.
//! `CFNotificationCenterGetDistributedCenter` provides the native system-wide IPC notification
//! mechanism, allowing any external tool to post [`ACTIVATION_NOTIFICATION`] across process
//! boundaries without requiring Mach ports, XPC daemons, or local sockets.
//!
//! # Claimed Hotkey Policy
//!
//! If another application has already registered a shortcut, `RegisterEventHotKey` returns
//! `eventHotKeyExistsErr` (-9878). That specific shortcut is unavailable, but the other
//! shortcut and the distributed notification listener still function.

use std::ffi::{c_char, c_void, CString};
use std::ptr;
use std::sync::OnceLock;

pub const ACTIVATION_NOTIFICATION: &str = "com.parsec-orbit.ui.toggle";

/// Carbon FourCC 'keyb' (0x6b657962).
const K_EVENT_CLASS_KEYBOARD: u32 = 0x6b657962;
/// Carbon kEventHotKeyPressed event kind (5).
const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
/// Carbon direct object parameter name '----' (0x2d2d2d2d).
const K_EVENT_PARAM_DIRECT_OBJECT: u32 = 0x2d2d2d2d;
/// Carbon hotkey parameter type 'hkid' (0x686b6964).
const TYPE_EVENT_HOT_KEY_ID: u32 = 0x686b6964;

/// FourCC identifier used for hotkeys registered by this module ('ORBT').
const HOTKEY_SIGNATURE: u32 = 0x4f524254;
/// ID for the primary hotkey (Control+Option+O).
const HOTKEY_ID_CONTROL_OPTION_O: u32 = 1;
/// ID for the fallback hotkey (Control+F12).
const HOTKEY_ID_CONTROL_F12: u32 = 2;

/// Key code for the 'O' key on macOS standard keyboard layout (0x1F / 31).
const KEY_CODE_O: u32 = 0x1F;
/// Key code for the F12 key on macOS standard keyboard layout (0x6F / 111).
const KEY_CODE_F12: u32 = 0x6F;

/// Carbon modifier flag for the Control key (controlKey, 0x1000 / 4096).
const MOD_CONTROL_KEY: u32 = 0x1000;
/// Carbon modifier flag for the Option/Alt key (optionKey, 0x0800 / 2048).
const MOD_OPTION_KEY: u32 = 0x0800;

/// HIToolbox error code returned when another program has already claimed a hotkey.
const EVENT_HOT_KEY_EXISTS_ERR: i32 = -9878;
/// Carbon error code indicating an event was not handled by this handler.
const EVENT_NOT_HANDLED_ERR: i32 = -9874;

/// UTF-8 string encoding identifier for CoreFoundation (kCFStringEncodingUTF8).
const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;
/// Suspension behavior delivering notifications immediately even when suspended.
const CF_NOTIFICATION_SUSPENSION_BEHAVIOR_DELIVER_IMMEDIATELY: isize = 4;

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
#[allow(non_snake_case)]
struct EventTypeSpec {
    eventClass: u32,
    eventKind: u32,
}

type EventHotKeyRef = *mut c_void;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventRef = *mut c_void;

type EventHandlerProcPtr = unsafe extern "C" fn(
    in_handler_call_ref: EventHandlerCallRef,
    in_event: EventRef,
    in_user_data: *mut c_void,
) -> i32;

type CFNotificationCenterRef = *mut c_void;
type CFStringRef = *const c_void;
type CFDictionaryRef = *const c_void;

type CFNotificationCallback = unsafe extern "C" fn(
    center: CFNotificationCenterRef,
    observer: *mut c_void,
    name: CFStringRef,
    object: *const c_void,
    user_info: CFDictionaryRef,
);

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn RegisterEventHotKey(
        in_hot_key_code: u32,
        in_hot_key_modifiers: u32,
        in_hot_key_id: EventHotKeyID,
        in_target: EventTargetRef,
        in_options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> i32;

    fn GetApplicationEventTarget() -> EventTargetRef;

    fn InstallEventHandler(
        in_target: EventTargetRef,
        in_handler: EventHandlerProcPtr,
        in_num_types: usize,
        in_list: *const EventTypeSpec,
        in_user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> i32;

    fn GetEventParameter(
        in_event: EventRef,
        in_name: u32,
        in_desired_type: u32,
        out_actual_type: *mut u32,
        in_buffer_size: usize,
        out_actual_size: *mut usize,
        out_data: *mut c_void,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFNotificationCenterGetDistributedCenter() -> CFNotificationCenterRef;
    fn CFNotificationCenterAddObserver(
        center: CFNotificationCenterRef,
        observer: *const c_void,
        callback: CFNotificationCallback,
        name: CFStringRef,
        object: *const c_void,
        suspension_behavior: isize,
    );
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFRelease(cf: *const c_void);
}

/// The callback invoked when a hotkey or distributed notification fires.
static TOGGLE: OnceLock<fn()> = OnceLock::new();
/// Tracks whether installation has succeeded to keep `install` idempotent.
static INSTALLED: OnceLock<()> = OnceLock::new();

/// Carbon event callback invoked on the main run loop when a registered hotkey is pressed.
unsafe extern "C" fn carbon_hotkey_handler(
    _in_handler_call_ref: EventHandlerCallRef,
    in_event: EventRef,
    _in_user_data: *mut c_void,
) -> i32 {
    let mut hkid = EventHotKeyID {
        signature: 0,
        id: 0,
    };
    let mut actual_size = 0usize;
    let status = unsafe {
        GetEventParameter(
            in_event,
            K_EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOT_KEY_ID,
            ptr::null_mut(),
            std::mem::size_of::<EventHotKeyID>(),
            &mut actual_size,
            (&mut hkid as *mut EventHotKeyID).cast(),
        )
    };

    // If another handler in the process registered a hotkey, yield to it.
    if status == 0 && hkid.signature != HOTKEY_SIGNATURE {
        return EVENT_NOT_HANDLED_ERR;
    }

    if let Some(toggle) = TOGGLE.get() {
        toggle();
    }
    0
}

/// CoreFoundation distributed notification callback invoked when an external process
/// broadcasts the activation notification.
unsafe extern "C" fn distributed_notification_callback(
    _center: CFNotificationCenterRef,
    _observer: *mut c_void,
    _name: CFStringRef,
    _object: *const c_void,
    _user_info: CFDictionaryRef,
) {
    if let Some(toggle) = TOGGLE.get() {
        toggle();
    }
}

/// Registers the two global hotkeys and the activation listener. Called once
/// from `ensure_started`, inside a main-queue block, after the window exists.
///
/// Idempotency: subsequent calls do nothing and return `Ok(())`, preserving the
/// initial registration for the lifetime of the process.
pub fn install(toggle: fn()) -> Result<(), String> {
    if INSTALLED.get().is_some() {
        return Ok(());
    }

    let _ = TOGGLE.set(toggle);

    let target = unsafe { GetApplicationEventTarget() };
    if target.is_null() {
        return Err("GetApplicationEventTarget returned null".to_string());
    }

    // 1. Register Control+Option+O.
    let hkid_primary = EventHotKeyID {
        signature: HOTKEY_SIGNATURE,
        id: HOTKEY_ID_CONTROL_OPTION_O,
    };
    let mut ref_primary: EventHotKeyRef = ptr::null_mut();
    let status_primary = unsafe {
        RegisterEventHotKey(
            KEY_CODE_O,
            MOD_CONTROL_KEY | MOD_OPTION_KEY,
            hkid_primary,
            target,
            0,
            &mut ref_primary,
        )
    };
    if status_primary != 0 && status_primary != EVENT_HOT_KEY_EXISTS_ERR {
        return Err(format!(
            "failed to register Control+Option+O hotkey: {status_primary}"
        ));
    }

    // 2. Register Control+F12.
    let hkid_fallback = EventHotKeyID {
        signature: HOTKEY_SIGNATURE,
        id: HOTKEY_ID_CONTROL_F12,
    };
    let mut ref_fallback: EventHotKeyRef = ptr::null_mut();
    let status_fallback = unsafe {
        RegisterEventHotKey(
            KEY_CODE_F12,
            MOD_CONTROL_KEY,
            hkid_fallback,
            target,
            0,
            &mut ref_fallback,
        )
    };
    if status_fallback != 0 && status_fallback != EVENT_HOT_KEY_EXISTS_ERR {
        return Err(format!(
            "failed to register Control+F12 hotkey: {status_fallback}"
        ));
    }

    // 3. Install Carbon event handler for hotkey pressed events.
    let spec = EventTypeSpec {
        eventClass: K_EVENT_CLASS_KEYBOARD,
        eventKind: K_EVENT_HOT_KEY_PRESSED,
    };
    let mut handler_ref: EventHandlerRef = ptr::null_mut();
    let status_handler = unsafe {
        InstallEventHandler(
            target,
            carbon_hotkey_handler,
            1,
            &spec,
            ptr::null_mut(),
            &mut handler_ref,
        )
    };
    if status_handler != 0 {
        return Err(format!(
            "failed to install Carbon event handler: {status_handler}"
        ));
    }

    // 4. Register cross-process activation listener via CFNotificationCenter.
    let center = unsafe { CFNotificationCenterGetDistributedCenter() };
    if center.is_null() {
        return Err("CFNotificationCenterGetDistributedCenter returned null".to_string());
    }

    let c_name = CString::new(ACTIVATION_NOTIFICATION)
        .map_err(|e| format!("invalid notification name: {e}"))?;
    let name_cf = unsafe {
        CFStringCreateWithCString(ptr::null(), c_name.as_ptr(), K_CF_STRING_ENCODING_UTF8)
    };
    if name_cf.is_null() {
        return Err("CFStringCreateWithCString returned null".to_string());
    }

    unsafe {
        CFNotificationCenterAddObserver(
            center,
            ptr::null(),
            distributed_notification_callback,
            name_cf,
            ptr::null(),
            CF_NOTIFICATION_SUSPENSION_BEHAVIOR_DELIVER_IMMEDIATELY,
        );
        CFRelease(name_cf);
    }

    let _ = INSTALLED.set(());
    Ok(())
}
