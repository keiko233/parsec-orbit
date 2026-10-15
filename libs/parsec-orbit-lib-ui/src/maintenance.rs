//! Windows elevation boundary for restart-and-apply.

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

/// Launches the adjacent maintenance helper with one UAC elevation request.
///
/// The helper receives this process's PID so it can validate and terminate
/// only the Parsec client that requested the restart.
#[cfg(windows)]
pub fn launch(helper: &Path, console: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_CANCELLED};
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };

    if !matches!(console, "off" | "1" | "client" | "host") {
        return Err(format!("invalid console mode: {console:?}"));
    }
    if !helper.is_absolute() {
        return Err("the maintenance helper path must be absolute".to_string());
    }

    let verb = wide("runas");
    let file: Vec<u16> = helper.as_os_str().encode_wide().chain(Some(0)).collect();
    let parameters = format!(
        "{} {} {}",
        quote_windows_argument(std::ffi::OsStr::new("--restart-and-apply")),
        quote_windows_argument(std::ffi::OsStr::new(console)),
        quote_windows_argument(std::ffi::OsStr::new(&std::process::id().to_string())),
    );
    let parameters = wide(&parameters);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        nShow: windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE,
        ..Default::default()
    };

    let ok = unsafe {
        // SAFETY: all strings and SHELLEXECUTEINFOW remain alive for the call.
        ShellExecuteExW(&mut info)
    };
    if ok == 0 {
        let code = unsafe { GetLastError() };
        if code == ERROR_CANCELLED {
            return Err("restart was cancelled at the Windows elevation prompt".to_string());
        }
        return Err(format!(
            "could not launch the elevated maintenance helper (Windows error {code})"
        ));
    }
    if !info.hProcess.is_null() {
        unsafe {
            // SAFETY: ShellExecuteExW returned this owned process handle.
            windows_sys::Win32::Foundation::CloseHandle(info.hProcess);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn quote_windows_argument(argument: &std::ffi::OsStr) -> String {
    let argument = argument.to_string_lossy();
    if !argument.is_empty() && !argument.chars().any(char::is_whitespace) && !argument.contains('"')
    {
        return argument.into_owned();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                quoted.push(character);
                backslashes = 0;
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_quoted_with_windows_command_line_rules() {
        assert_eq!(
            quote_windows_argument(std::ffi::OsStr::new("simple")),
            "simple"
        );
        assert_eq!(
            quote_windows_argument(std::ffi::OsStr::new("two words")),
            "\"two words\""
        );
        assert_eq!(
            quote_windows_argument(std::ffi::OsStr::new("a\\\"b")),
            "\"a\\\\\\\"b\""
        );
        assert_eq!(
            quote_windows_argument(std::ffi::OsStr::new("dir with space\\")),
            "\"dir with space\\\\\""
        );
    }
}
