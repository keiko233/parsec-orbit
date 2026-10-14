//! The window's entry points from outside Parsec.
//!
//! Two global hotkeys toggle the companion window, and a hidden activation
//! window lets `scripts/orbit-manager.ps1 ui` ask for the same thing without
//! guessing at keystrokes: the script posts [`ACTIVATION_MESSAGE_NAME`] to
//! [`ACTIVATION_CLASS`] and the message loop below runs the same toggle the
//! hotkeys do.
//!
//! A hotkey another program has already claimed only costs that one shortcut;
//! the rest still work, so the thread never gives up on the first failure.

use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, TranslateMessage, MSG, WM_DESTROY, WM_HOTKEY,
    WNDCLASSW, WS_OVERLAPPED,
};

/// Class name of the hidden window the CLI script posts to.
pub const ACTIVATION_CLASS: &str = "ParsecOrbitUiActivation";
/// Registered message name the CLI script asks the platform to number.
pub const ACTIVATION_MESSAGE_NAME: &str = "PARSEC_ORBIT_UI_TOGGLE";

/// `Ctrl+Alt+O`; Parsec itself does not claim it.
const HOTKEY_ID: i32 = 1;
const VK_O: u32 = 0x4F;
/// `Ctrl+F12`, a second way in for keyboards or layouts where the first fails.
const ALT_HOTKEY_ID: i32 = 2;
const VK_F12: u32 = 0x7B;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return 0;
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// Registers the activation window and both hotkeys, then pumps messages until
/// the window closes. `toggle` runs on this thread.
pub fn run(toggle: impl Fn() + Send + 'static) {
    unsafe {
        let instance = GetModuleHandleW(null_mut());
        let class = wide(ACTIVATION_CLASS);
        let registration = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null_mut(),
            lpszClassName: class.as_ptr(),
        };
        if RegisterClassW(&registration) == 0 {
            return;
        }

        let activation = RegisterWindowMessageW(wide(ACTIVATION_MESSAGE_NAME).as_ptr());
        let activation_window = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Parsec Orbit Activation").as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null_mut(),
        );
        if activation_window.is_null() {
            return;
        }

        // RegisterHotKey returns 0 when another program owns the combination;
        // that shortcut is lost, the others are not.
        RegisterHotKey(
            activation_window,
            HOTKEY_ID,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            VK_O,
        );
        RegisterHotKey(
            activation_window,
            ALT_HOTKEY_ID,
            MOD_CONTROL | MOD_NOREPEAT,
            VK_F12,
        );

        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            let is_toggle =
                message.message == WM_HOTKEY || (activation != 0 && message.message == activation);
            if is_toggle {
                toggle();
            } else {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        UnregisterHotKey(activation_window, HOTKEY_ID);
        UnregisterHotKey(activation_window, ALT_HOTKEY_ID);
    }
}
