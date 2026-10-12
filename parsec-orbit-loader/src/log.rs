//! Line oriented logging.
//!
//! The loader writes with raw `CreateFileW`/`WriteFile` calls and never
//! panics: it can run before the client has finished initialising, so it must
//! not depend on anything that could still be half set up.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, SYSTEMTIME,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, WriteFile, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_ALWAYS,
};
use windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringW;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static PROCESS_NAME: OnceLock<String> = OnceLock::new();

/// Result of [`init`].
pub struct Init {
    /// File every line is appended to; `None` when no candidate accepted a
    /// write, in which case lines only reach the debugger output.
    pub path: Option<PathBuf>,
    /// Candidates that could not be opened, with their Win32 error code.
    pub rejected: Vec<(PathBuf, u32)>,
}

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

/// Switches logging on by picking the first candidate that can actually be
/// written to. Callers pass more than one because a single path can be
/// unusable: `%APPDATA%` is missing in service environments, the client
/// directory needs elevation, and another process holding the file without
/// sharing write is enough to make a specific path fail for a moment.
pub fn init(candidates: &[PathBuf]) -> Init {
    let mut rejected = Vec::new();
    for candidate in candidates {
        // The default location is `%APPDATA%\ParsecOrbit`, which does not
        // exist on a fresh machine; CreateFileW alone would silently give up.
        if let Some(parent) = candidate.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match probe(candidate) {
            Ok(()) => {
                let _ = LOG_PATH.set(candidate.clone());
                return Init {
                    path: Some(candidate.clone()),
                    rejected,
                };
            }
            Err(code) => rejected.push((candidate.clone(), code)),
        }
    }
    Init {
        path: None,
        rejected,
    }
}

/// Opens the file for writing without truncating it, so an unusable candidate
/// is detected at start-up instead of losing the first log line.
fn probe(path: &Path) -> Result<(), u32> {
    let name = wide(&path.to_string_lossy());
    let handle: HANDLE = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(unsafe { GetLastError() });
    }
    unsafe { CloseHandle(handle) };
    Ok(())
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

/// Appends one line. Every line is also mirrored to the debugger output, and a
/// failed write is retried: a reader that did not share write access is enough
/// to make a single attempt fail.
fn write_line(path: Option<&Path>, line: &str) {
    if let Some(path) = path {
        let name = wide(&path.to_string_lossy());
        for attempt in 0..3 {
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
            if handle != INVALID_HANDLE_VALUE {
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
                break;
            }
            if attempt < 2 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
    let text: Vec<u16> = OsStr::new(line)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe { OutputDebugStringW(text.as_ptr()) };
}

macro_rules! level_fn {
    ($name:ident, $tag:literal) => {
        /// Writes one log line to the log file and the debugger output.
        pub fn $name(message: &str) {
            let (month, day, hour, minute, second, millisecond) = timestamp();
            write_line(
                LOG_PATH.get().map(PathBuf::as_path),
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
