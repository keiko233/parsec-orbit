//! The loader log line format, shared by every loader so one log reader works
//! for all platforms. Writing the line is the loader's business (raw
//! `WriteFile` on Windows, `open(O_APPEND)` on macOS); only the text is here.

/// Local time of a log line: month, day, hour, minute, second, millisecond.
pub type Timestamp = (u16, u16, u16, u16, u16, u16);

/// `[MM-DD hh:mm:ss.mmm] [tag] [process pid=N] message`, newline terminated.
pub fn format_line(
    (month, day, hour, minute, second, millisecond): Timestamp,
    tag: &str,
    process: &str,
    pid: u32,
    message: &str,
) -> String {
    format!(
        "[{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{millisecond:03}] [{tag}] [{process} pid={pid}] {message}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
