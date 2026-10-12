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

/// `[MM-DD hh:mm:ss.mmm] [tag] [process pid=N] message`, newline terminated.
fn format_line(
    (month, day, hour, minute, second, millisecond): (u16, u16, u16, u16, u16, u16),
    tag: &str,
    process: &str,
    pid: u32,
    message: &str,
) -> String {
    format!(
        "[{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{millisecond:03}] [{tag}] [{process} pid={pid}] {message}\n"
    )
}

macro_rules! level_fn {
    ($name:ident, $tag:literal) => {
        /// Writes one log line to the log file and the debugger output.
        pub fn $name(message: &str) {
            write_line(
                LOG_PATH.get().map(PathBuf::as_path),
                &format_line(
                    timestamp(),
                    $tag,
                    process_name(),
                    std::process::id(),
                    message,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const ERROR_PATH_NOT_FOUND: u32 = 3;
    const ERROR_ACCESS_DENIED: u32 = 5;

    #[test]
    fn line_format_is_stable() {
        assert_eq!(
            format_line((3, 7, 4, 5, 6, 89), "info", "parsecd.exe", 42, "hello"),
            "[03-07 04:05:06.089] [info] [parsecd.exe pid=42] hello\n"
        );
        assert_eq!(
            format_line((12, 31, 23, 59, 59, 999), "error", "x.exe", 4_000_000, ""),
            "[12-31 23:59:59.999] [error] [x.exe pid=4000000] \n"
        );
    }

    #[test]
    fn timestamp_is_a_plausible_local_time() {
        let (month, day, hour, minute, second, millisecond) = timestamp();
        assert!((1..=12).contains(&month));
        assert!((1..=31).contains(&day));
        assert!(hour < 24 && minute < 60 && second < 61 && millisecond < 1000);
    }

    #[test]
    fn wide_is_nul_terminated_utf16() {
        assert_eq!(wide(""), [0]);
        assert_eq!(wide("a\u{30ed}"), [0x61, 0x30ed, 0]);
        // Outside the BMP: a surrogate pair.
        assert_eq!(wide("\u{1F600}"), [0xD83D, 0xDE00, 0]);
    }

    #[test]
    fn process_name_is_the_bare_executable_name() {
        let name = process_name();
        assert!(!name.is_empty());
        assert!(!name.contains('\\') && !name.contains('/'), "{name}");
        assert!(name.to_ascii_lowercase().ends_with(".exe"), "{name}");
    }

    #[test]
    fn probe_accepts_a_new_file_and_does_not_truncate_an_existing_one() {
        let dir = TempDir::new("probe");
        let fresh = dir.path().join("fresh.log");
        assert_eq!(probe(&fresh), Ok(()));
        assert!(fresh.is_file(), "the probe creates the file");

        let existing = dir.write("existing.log", "keep me\n");
        assert_eq!(probe(&existing), Ok(()));
        assert_eq!(std::fs::read_to_string(existing).unwrap(), "keep me\n");
    }

    #[test]
    fn probe_reports_the_win32_error_for_unusable_paths() {
        let dir = TempDir::new("probe-bad");
        assert_eq!(
            probe(&dir.path().join("no-such-dir").join("x.log")),
            Err(ERROR_PATH_NOT_FOUND)
        );
        assert_eq!(probe(dir.path()), Err(ERROR_ACCESS_DENIED));
    }

    #[test]
    fn write_line_appends_in_order() {
        let dir = TempDir::new("write");
        let path = dir.path().join("a.log");
        write_line(Some(&path), "first\n");
        write_line(Some(&path), "second \u{30ed}\n");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "first\nsecond \u{30ed}\n"
        );
    }

    #[test]
    fn write_line_without_a_usable_file_does_not_panic() {
        write_line(None, "debugger output only\n");
        let dir = TempDir::new("write-bad");
        // A directory cannot be opened for append; every retry fails quietly.
        write_line(Some(dir.path()), "lost\n");
        write_line(Some(&dir.path().join("missing").join("x.log")), "lost\n");
    }

    #[test]
    fn concurrent_writers_never_interleave_inside_a_line() {
        let dir = TempDir::new("write-threads");
        let path = dir.path().join("threads.log");
        let threads: Vec<_> = (0..8)
            .map(|thread| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for index in 0..50 {
                        write_line(
                            Some(&path),
                            &format!("t{thread} line {index} {}\n", "x".repeat(200)),
                        );
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 8 * 50);
        for line in lines {
            assert!(
                line.starts_with('t') && line.ends_with(&"x".repeat(200)),
                "{line}"
            );
        }
    }

    #[test]
    fn init_without_candidates_leaves_logging_off() {
        let result = init(&[]);
        assert_eq!(result.path, None);
        assert!(result.rejected.is_empty());
    }

    /// The only test that activates the process-wide log file: `init` can
    /// succeed once per process.
    #[test]
    fn init_skips_unwritable_candidates_and_then_logs_every_level() {
        let dir = TempDir::new("init");
        let blocked = dir.path().to_path_buf(); // a directory is not writable as a file
        let good = dir
            .path()
            .join("nested")
            .join("deeper")
            .join("parsec-orbit.log");
        let unused = dir.path().join("unused.log");

        let result = init(&[blocked.clone(), good.clone(), unused.clone()]);

        assert_eq!(result.path.as_deref(), Some(good.as_path()));
        assert_eq!(result.rejected, vec![(blocked, ERROR_ACCESS_DENIED)]);
        assert!(good.is_file(), "missing parent directories are created");
        assert!(
            !unused.exists(),
            "candidates after the winner are never touched"
        );

        by_level(0, "level-test trace");
        by_level(1, "level-test debug");
        by_level(2, "level-test info");
        by_level(3, "level-test warn");
        by_level(4, "level-test error");
        by_level(99, "level-test unknown");
        trace("direct trace");

        let content = std::fs::read_to_string(&good).unwrap();
        let tag_of = |message: &str| {
            let line = content
                .lines()
                .find(|line| line.ends_with(message))
                .unwrap_or_else(|| panic!("no line for {message:?} in:\n{content}"));
            // [MM-DD hh:mm:ss.mmm] [tag] [process pid=N] message
            assert_eq!(line.as_bytes()[0], b'[');
            assert_eq!(line.as_bytes()[3], b'-');
            assert_eq!(line.as_bytes()[9], b':');
            assert_eq!(line.as_bytes()[15], b'.');
            assert!(
                line.contains(&format!("[{} pid={}]", process_name(), std::process::id())),
                "{line}"
            );
            line.split("] [").nth(1).unwrap().to_string()
        };
        assert_eq!(tag_of("level-test trace"), "trace");
        assert_eq!(tag_of("level-test debug"), "debug");
        assert_eq!(tag_of("level-test info"), "info");
        assert_eq!(tag_of("level-test warn"), "warn");
        assert_eq!(tag_of("level-test error"), "error");
        assert_eq!(
            tag_of("level-test unknown"),
            "info",
            "unknown levels log as info"
        );
        assert_eq!(tag_of("direct trace"), "trace");
    }
}
