//! Line oriented logging, the macOS half.
//!
//! Lines have the same format as the Windows loader's, so one log reader
//! works for both platforms:
//!
//! ```text
//! [MM-DD hh:mm:ss.mmm] [tag] [process pid=N] message
//! ```
//!
//! Every line is one `O_APPEND` write, and debug builds mirror it to stderr.
//! The module never panics: it runs while the client process is still coming
//! up, so it must not depend on shared state that may be half initialised.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use parsec_orbit_core::Level;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static PROCESS_NAME: OnceLock<String> = OnceLock::new();

/// Result of [`init`].
pub struct Init {
    /// File every line is appended to; `None` when no candidate accepted a
    /// write, in which case lines only reach stderr in debug builds.
    pub path: Option<PathBuf>,
    /// Candidates that could not be opened, with the errno.
    pub rejected: Vec<(PathBuf, i32)>,
}

/// Base name of the executable this library is mapped into.
pub fn process_name() -> &'static str {
    PROCESS_NAME.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "unknown".to_string())
    })
}

fn errno() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

/// Opens the file for appending without truncating it.
fn open_append(path: &Path) -> Result<i32, i32> {
    let name = match CString::new(path.as_os_str().as_bytes()) {
        Ok(name) => name,
        Err(_) => return Err(libc::EINVAL),
    };
    // Safety: the path is a valid NUL-terminated C string, the flags need no
    // further arguments.
    let fd = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC,
            0o644,
        )
    };
    if fd < 0 {
        return Err(errno());
    }
    Ok(fd)
}

/// Opens the file for writing without truncating it, so an unusable candidate
/// is detected at start-up instead of losing the first log line.
fn probe(path: &Path) -> Result<(), i32> {
    let fd = open_append(path)?;
    unsafe { libc::close(fd) };
    Ok(())
}

/// Switches logging on by picking the first candidate that can actually be
/// written to. Callers pass more than one because a single path can be
/// unusable: the Orbit root may live on a read-only volume or be missing
/// entirely on a fresh machine, and another process holding the file without
/// sharing write is enough to make a specific path fail for a moment.
pub fn init(candidates: &[PathBuf]) -> Init {
    let mut rejected = Vec::new();
    for candidate in candidates {
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

/// `MM-DD hh:mm:ss.mmm` in local time.
fn timestamp() -> (u16, u16, u16, u16, u16, u16) {
    let mut now = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    // Safety: `now` points to a valid `timeval`.
    unsafe { libc::gettimeofday(&mut now, std::ptr::null_mut()) };
    let seconds = now.tv_sec as libc::time_t;
    let mut local = libc::tm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null_mut(),
    };
    // Safety: `seconds` and `local` are valid; `localtime_r` fills `local`.
    let filled = unsafe { libc::localtime_r(&seconds, &mut local) };
    if filled.is_null() {
        return (0, 0, 0, 0, 0, 0);
    }
    (
        (local.tm_mon + 1) as u16,
        local.tm_mday as u16,
        local.tm_hour as u16,
        local.tm_min as u16,
        local.tm_sec as u16,
        (now.tv_usec / 1000) as u16,
    )
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

/// Appends one line. A failed write is retried: a reader that did not share
/// the file handle is enough to make a single attempt fail.
fn write_line(path: Option<&Path>, line: &str) {
    if let Some(path) = path {
        for attempt in 0..3 {
            match open_append(path) {
                Ok(fd) => {
                    // Safety: the fd is open for writing and the whole line is
                    // handed over in one write, so concurrent writers do not
                    // interleave inside a line.
                    unsafe {
                        libc::write(fd, line.as_ptr() as *const libc::c_void, line.len());
                        libc::close(fd);
                    }
                    break;
                }
                Err(_) => {
                    if attempt < 2 {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
            }
        }
    }
    #[cfg(debug_assertions)]
    {
        // Debug builds mirror to stderr; release builds only write the file.
        eprint!("{line}");
    }
}

/// Writes one log line with the level's tag.
pub fn log(level: Level, message: &str) {
    write_line(
        LOG_PATH.get().map(PathBuf::as_path),
        &format_line(
            timestamp(),
            level.as_str(),
            process_name(),
            std::process::id(),
            message,
        ),
    );
}

pub fn debug(message: &str) {
    log(Level::Debug, message);
}
pub fn info(message: &str) {
    log(Level::Info, message);
}
pub fn warn(message: &str) {
    log(Level::Warn, message);
}
pub fn error(message: &str) {
    log(Level::Error, message);
}

/// Reads back the file logging was switched to, for tests and diagnostics.
pub fn active_path() -> Option<&'static Path> {
    LOG_PATH.get().map(PathBuf::as_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_core::testutil::TempDir;

    const ENOENT: i32 = libc::ENOENT;
    const EISDIR: i32 = libc::EISDIR;

    #[test]
    fn line_format_is_the_windows_one() {
        assert_eq!(
            format_line((3, 7, 4, 5, 6, 89), "info", "parsecd", 42, "hello"),
            "[03-07 04:05:06.089] [info] [parsecd pid=42] hello\n"
        );
        assert_eq!(
            format_line((12, 31, 23, 59, 59, 999), "error", "x", 4_000_000, ""),
            "[12-31 23:59:59.999] [error] [x pid=4000000] \n"
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
    fn process_name_is_the_bare_executable_name() {
        let name = process_name();
        assert!(!name.is_empty());
        assert!(!name.contains('/'), "{name}");
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
    fn probe_reports_errno_for_unusable_paths() {
        let dir = TempDir::new("probe-bad");
        assert_eq!(
            probe(&dir.path().join("no-such-dir").join("x.log")),
            Err(ENOENT)
        );
        assert_eq!(probe(dir.path()), Err(EISDIR));
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
        write_line(None, "stderr only\n");
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
    /// succeed once per process. It runs in the lib test binary, which other
    /// tests share — none of them call [`init`].
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
        assert_eq!(result.rejected, vec![(blocked, EISDIR)]);
        assert!(good.is_file(), "missing parent directories are created");
        assert!(
            !unused.exists(),
            "candidates after the winner are never touched"
        );
        assert_eq!(active_path(), Some(good.as_path()));

        log(Level::from_abi(0), "level-test trace");
        log(Level::from_abi(1), "level-test debug");
        log(Level::from_abi(2), "level-test info");
        log(Level::from_abi(3), "level-test warn");
        log(Level::from_abi(4), "level-test error");
        log(Level::from_abi(99), "level-test unknown");

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
    }
}
