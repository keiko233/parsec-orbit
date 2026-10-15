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

/// Value the loader puts into the environment for a given port.
fn browser_arguments(port: u16) -> String {
    format!("--remote-debugging-port={port} --remote-allow-origins=*")
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

    let value = wide(&browser_arguments(port));
    let ok = unsafe { SetEnvironmentVariableW(name.as_ptr(), value.as_ptr()) };
    if ok != 0 {
        log::info(&format!(
            "devtools enabled: http://127.0.0.1:{port}/json (WebView2 UI only)"
        ));
    } else {
        log::warn("failed to set the WebView2 browser arguments");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current() -> Option<String> {
        std::env::var(VARIABLE).ok()
    }

    #[test]
    fn browser_arguments_open_the_port_and_allow_any_origin() {
        assert_eq!(
            browser_arguments(9222),
            "--remote-debugging-port=9222 --remote-allow-origins=*"
        );
        assert_eq!(
            browser_arguments(1),
            "--remote-debugging-port=1 --remote-allow-origins=*"
        );
        assert_eq!(
            browser_arguments(65535),
            "--remote-debugging-port=65535 --remote-allow-origins=*"
        );
    }

    /// The only test that touches `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, so
    /// the steps can safely share the process environment.
    #[test]
    fn prepare_sets_the_variable_once_and_never_overwrites_the_users_value() {
        std::env::remove_var(VARIABLE);
        prepare(9333);
        assert_eq!(
            current().as_deref(),
            Some("--remote-debugging-port=9333 --remote-allow-origins=*")
        );

        // A second call (or a user value) is left alone.
        prepare(9444);
        assert_eq!(
            current().as_deref(),
            Some("--remote-debugging-port=9333 --remote-allow-origins=*")
        );

        std::env::set_var(VARIABLE, "--user-flag");
        prepare(9555);
        assert_eq!(current().as_deref(), Some("--user-flag"));

        // Longer than the 512 character probe buffer: still counts as set.
        let long = format!("--user-flag={}", "x".repeat(2000));
        std::env::set_var(VARIABLE, &long);
        prepare(9666);
        assert_eq!(current().as_deref(), Some(long.as_str()));

        std::env::remove_var(VARIABLE);
    }
}
