//! Line oriented logging.
//!
//! The loader writes with raw `CreateFileW`/`WriteFile` calls and never
//! panics: it can run before the client has finished initialising, so it must
//! not depend on anything that could still be half set up.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, SYSTEMTIME};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, WriteFile, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_ALWAYS,
};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static PROCESS_NAME: OnceLock<String> = OnceLock::new();

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Name of the executable this module is loaded into.
pub fn process_name() -> &'static str {
    PROCESS_NAME.get_or_init(|| {
        let mut buffer = [0u16; 1024];
        let len = unsafe {
            windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW(
                std::ptr::null_mut(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
            )
        };
        String::from_utf16_lossy(&buffer[..len as usize])
            .rsplit('\\')
            .next()
            .unwrap_or("unknown")
            .to_string()
    })
}

/// Switches logging on. Without it every call site is a no-op.
pub fn init(path: PathBuf) {
    let _ = LOG_PATH.set(path);
}

fn timestamp() -> (u16, u16, u16, u16, u16, u16) {
    let mut time = SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    unsafe { GetLocalTime(&mut time) };
    (
        time.wMonth,
        time.wDay,
        time.wHour,
        time.wMinute,
        time.wSecond,
        time.wMilliseconds,
    )
}

fn write_line(path: &Path, line: &str) {
    let name = wide(&path.to_string_lossy());
    let handle: HANDLE = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_APPEND_DATA,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return;
    }
    let mut written = 0u32;
    unsafe {
        WriteFile(
            handle,
            line.as_ptr(),
            line.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        );
        CloseHandle(handle);
    }
}

macro_rules! level_fn {
    ($name:ident, $tag:literal) => {
        /// Writes one log line; does nothing when logging is not initialised.
        pub fn $name(message: &str) {
            let Some(path) = LOG_PATH.get() else {
                return;
            };
            let (month, day, hour, minute, second, millisecond) = timestamp();
            write_line(
                path,
                &format!(
                    "[{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{millisecond:03}] [{tag}] [{} pid={}] {message}\n",
                    process_name(),
                    std::process::id(),
                    tag = $tag
                ),
            );
        }
    };
}

level_fn!(trace, "trace");
level_fn!(debug, "debug");
level_fn!(info, "info");
level_fn!(warn, "warn");
level_fn!(error, "error");

/// Maps the numeric levels used by the plugin ABI onto the log tags.
pub fn by_level(level: u32, message: &str) {
    match level {
        0 => trace(message),
        1 => debug(message),
        3 => warn(message),
        4 => error(message),
        _ => info(message),
    }
}
