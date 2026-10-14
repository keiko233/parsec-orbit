//! Global hotkey listener for Ctrl+Shift+O.

use std::ptr::null_mut;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

const HOTKEY_ID: i32 = 1;
const VK_O: u32 = 0x4F;

pub fn run(toggle: impl Fn() + Send + 'static) {
    unsafe {
        let ok = RegisterHotKey(
            null_mut(),
            HOTKEY_ID,
            MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
            VK_O,
        );
        if ok == 0 {
            return;
        }

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if msg.message == WM_HOTKEY && msg.wParam as i32 == HOTKEY_ID {
                toggle();
            }
        }
        UnregisterHotKey(null_mut(), HOTKEY_ID);
    }
}
