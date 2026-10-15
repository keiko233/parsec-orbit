//! Debug console: an opt-in window that shows the loader log.
//!
//! The window itself is Win32 (`AllocConsole`) and lives in the loader; this
//! module is the platform-independent half:
//!
//! * [`Console`] is the sink the loader implements for its console window,
//! * [`install`] and [`print_line`] hold that sink process-wide, the same way
//!   `host` holds the loader's logger (`Logger`),
//! * [`target`] turns the raw `PARSEC_ORBIT_CONSOLE` value into the
//!   instances that should open one,
//! * [`Follower`] mirrors an appended file - Parsec's own log - into the
//!   console, so the window shows the loader and the client side by side.
//!
//! The console is best effort by design: it is a debug aid, so nothing here
//! may fail the load, block it, or panic. Without an installed console every
//! line is dropped.

use std::path::PathBuf;
use std::sync::OnceLock;

pub use parsec_orbit_log_tail::{Follower, FOLLOW_INTERVAL};

/// A window that displays log lines, implemented by the loader.
pub trait Console {
    /// Displays one line. `line` carries no trailing line break; the
    /// implementation adds one. Called from any thread, must not panic and
    /// should not block on anything but the console write itself.
    fn print_line(&self, line: &str);
}

/// The sink the loader installed, if any.
static CONSOLE: OnceLock<&'static (dyn Console + Sync)> = OnceLock::new();

/// Sets the console sink. The first call wins; later calls are ignored, so a
/// second caller cannot take over a console the process already has.
pub fn install(console: &'static (dyn Console + Sync)) {
    let _ = CONSOLE.set(console);
}

/// Whether a console sink was installed.
pub fn installed() -> bool {
    CONSOLE.get().is_some()
}

/// One line to the console, if one is installed.
pub fn print_line(line: &str) {
    if let Some(console) = CONSOLE.get() {
        console.print_line(line);
    }
}

/// Which instances open a console window.
///
/// A machine that runs the client normally has two of them: the instance the
/// Parsec service starts for hosting, and the one the user starts. A Windows
/// console belongs to a process, so `All` means one window per instance; the
/// narrower values pick one side, which is what keeps a debug session down to
/// a single window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// No window anywhere.
    Off,
    /// A window in every process that loads the loader.
    All,
    /// Only in the instance the user started, not the service's.
    Client,
    /// Only in the instance the service started, which runs as `LocalSystem`.
    Host,
}

impl Target {
    /// The value this target is spelled with in `PARSEC_ORBIT_CONSOLE`.
    pub fn as_str(self) -> &'static str {
        match self {
            Target::Off => "off",
            Target::All => "1",
            Target::Client => "client",
            Target::Host => "host",
        }
    }
}

/// The console a raw `PARSEC_ORBIT_CONSOLE` value asks for.
///
/// The setting is opt-in and the values are spelled exactly: `1`, `on`,
/// `true` and `yes` mean every instance, `client` and `host` narrow it to
/// one, and everything else (including an unset variable) leaves the console
/// off.
pub fn target(value: Option<&str>) -> Target {
    match value {
        Some("1" | "on" | "true" | "yes") => Target::All,
        Some("client") => Target::Client,
        Some("host") => Target::Host,
        _ => Target::Off,
    }
}

/// Prefix of a mirrored line, so Parsec's lines cannot be mistaken for the
/// loader's own `[MM-DD hh:mm:ss.mmm] [tag] [process pid=N]` lines.
const FOLLOW_TAG: &str = "[parsec] ";

/// Mirrors every line appended to `paths` to the installed console, tagged
/// with `[parsec]`, until the process exits: this call does not return.
///
/// Files that do not exist (yet) are followed as soon as they appear. Without
/// an installed console there is nothing to show and it returns at once.
///
/// The caller owns the thread. The loader starts this on the thread it
/// already knows how to create rather than through `std::thread`, whose
/// runtime would cost the DLL about 20 KB of its size budget.
pub fn follow_forever(paths: Vec<PathBuf>) {
    if !installed() {
        return;
    }
    let mut followers: Vec<Follower> = paths.into_iter().map(Follower::new).collect();
    loop {
        for follower in &mut followers {
            for line in follower.poll() {
                print_line(&format!("{FOLLOW_TAG}{line}"));
            }
        }
        std::thread::sleep(FOLLOW_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ---- sink --------------------------------------------------------------

    struct Recorder;

    static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static FIRST: Recorder = Recorder;
    static SECOND: Recorder = Recorder;

    impl Console for Recorder {
        fn print_line(&self, line: &str) {
            LINES.lock().unwrap().push(line.to_string());
        }
    }

    /// The only test that installs a console: the slot takes one per process.
    #[test]
    fn the_first_installed_console_gets_the_lines() {
        assert!(!installed(), "no other test may install a console");
        install(&FIRST);
        install(&SECOND);
        assert!(installed());

        print_line("hello");
        assert_eq!(*LINES.lock().unwrap(), ["hello"]);
    }

    // ---- the setting -------------------------------------------------------

    #[test]
    fn the_console_setting_is_opt_in_and_spelled_exactly() {
        for value in ["1", "on", "true", "yes"] {
            assert_eq!(
                target(Some(value)),
                Target::All,
                "PARSEC_ORBIT_CONSOLE={value}"
            );
        }
        assert_eq!(target(Some("client")), Target::Client);
        assert_eq!(target(Some("host")), Target::Host);
        for other in [
            None,
            Some(""),
            Some("0"),
            Some("off"),
            Some("false"),
            Some("ON"),
            Some("Client"),
            Some("any"),
            Some("all"),
        ] {
            assert_eq!(target(other), Target::Off, "PARSEC_ORBIT_CONSOLE={other:?}");
        }
    }

    #[test]
    fn every_target_parses_back_from_the_value_it_is_spelled_with() {
        for spelled in [Target::All, Target::Client, Target::Host] {
            assert_eq!(target(Some(spelled.as_str())), spelled);
        }
        assert_eq!(target(None).as_str(), "off", "the default has no value");
    }
}
