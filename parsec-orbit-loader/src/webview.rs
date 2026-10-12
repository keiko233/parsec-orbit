//! WebView2 preparation.
//!
//! The client can render its interface with WebView2. Setting the additional
//! browser arguments before the client creates the environment opens a local
//! devtools endpoint, which is what makes the UI inspectable.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::System::Environment::{GetEnvironmentVariableW, SetEnvironmentVariableW};

use crate::log;

const VARIABLE: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Adds `--remote-debugging-port=<port>` unless the user already configured
/// that variable themselves.
pub fn prepare(port: u16) {
    let name = wide(VARIABLE);
    let mut existing = [0u16; 512];
    let len = unsafe {
        GetEnvironmentVariableW(name.as_ptr(), existing.as_mut_ptr(), existing.len() as u32)
    };
    if len > 0 {
        log::info(&format!(
            "{VARIABLE} already set ({} characters), left untouched",
            len
        ));
        return;
    }

    let value = wide(&format!(
        "--remote-debugging-port={port} --remote-allow-origins=*"
    ));
    let ok = unsafe { SetEnvironmentVariableW(name.as_ptr(), value.as_ptr()) };
    if ok != 0 {
        log::info(&format!(
            "devtools enabled: http://127.0.0.1:{port}/json (WebView2 UI only)"
        ));
    } else {
        log::warn("failed to set the WebView2 browser arguments");
    }
}
