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

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

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

// ---- mirroring Parsec's own log -------------------------------------------

/// How often [`follow`] polls the files it mirrors.
pub const FOLLOW_INTERVAL: Duration = Duration::from_millis(200);

/// Prefix of a mirrored line, so Parsec's lines cannot be mistaken for the
/// loader's own `[MM-DD hh:mm:ss.mmm] [tag] [process pid=N]` lines.
const FOLLOW_TAG: &str = "[parsec] ";

/// A file that is at most this large and was written within [`FRESH`] is the
/// current run's freshly rotated log: its first lines were written before the
/// console existed, so the beginning is shown too. Older or larger files are
/// a previous run and are followed from their end only.
const SMALL: u64 = 64 * 1024;
const FRESH: Duration = Duration::from_secs(15);

/// A file that grew without a line break for longer than this is emitted as a
/// line instead of buffering without bound.
const PENDING_LIMIT: usize = 64 * 1024;

/// How many bytes of the last chunk read are remembered to recognize the file
/// again (see [`Follower::anchor_holds`]).
const ANCHOR: usize = 32;

/// Follows one appended file and yields the complete lines that appeared.
///
/// The file is opened, read and closed on every poll and never held open, so
/// the process that writes it can rotate or rename it at any time. A file
/// that shrank, or was replaced by a new file with the same name, is read
/// from the beginning again: that is what a rotation looks like from here.
pub struct Follower {
    path: PathBuf,
    /// Bytes read so far; the next poll starts here.
    position: u64,
    /// The last bytes of the data `position` was counted from, and where they
    /// were, so a file that was replaced under the same name is recognized
    /// even when it is already longer than `position` again.
    anchor_at: u64,
    anchor: Vec<u8>,
    /// Bytes of an incomplete line that have been read but not yet yielded.
    pending: Vec<u8>,
}

impl Follower {
    /// Starts following `path`, from the beginning for a small file that was
    /// just written, from its end otherwise.
    pub fn new(path: PathBuf) -> Follower {
        let mut follower = Follower {
            path,
            position: 0,
            anchor_at: 0,
            anchor: Vec::new(),
            pending: Vec::new(),
        };
        if let Ok(metadata) = std::fs::metadata(&follower.path) {
            follower.position = start_position(&metadata);
        }
        follower
    }

    /// The file this follower reads.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The complete lines appended since the last poll. An incomplete trailing
    /// line is kept until its line break shows up.
    pub fn poll(&mut self) -> Vec<String> {
        let Ok(metadata) = std::fs::metadata(&self.path) else {
            // Not created yet (Parsec writes it after the console opens) or
            // gone again; either way there is nothing to read right now.
            return Vec::new();
        };

        if metadata.len() < self.position {
            // Truncated in place, typically the rotation of a fresh run.
            self.restart();
        }

        let Ok(mut file) = std::fs::File::open(&self.path) else {
            return Vec::new();
        };
        if !self.anchor_holds(&mut file) {
            // The bytes read last are not where they were: a new file with
            // the same name, already past `position`. Start over.
            self.restart();
        }
        if metadata.len() <= self.position {
            return Vec::new();
        }
        if file.seek(SeekFrom::Start(self.position)).is_err() {
            return Vec::new();
        }
        let remaining = (metadata.len() - self.position) as usize;
        let mut chunk = vec![0u8; remaining];
        if file.read_exact(&mut chunk).is_err() {
            return Vec::new();
        }
        self.position += chunk.len() as u64;
        self.set_anchor(&chunk);
        self.pending.extend_from_slice(&chunk);
        self.take_lines()
    }

    /// Reads from the beginning again.
    fn restart(&mut self) {
        self.position = 0;
        self.anchor.clear();
        self.pending.clear();
    }

    /// Remembers the end of the chunk just read as the anchor.
    fn set_anchor(&mut self, chunk: &[u8]) {
        let take = chunk.len().min(ANCHOR);
        self.anchor_at = self.position - take as u64;
        self.anchor.clear();
        self.anchor.extend_from_slice(&chunk[chunk.len() - take..]);
    }

    /// Whether the anchored bytes are still at the anchored offset. A file
    /// that was replaced under the same name fails this, even if it has
    /// already grown past `position`.
    fn anchor_holds(&mut self, file: &mut std::fs::File) -> bool {
        if self.anchor.is_empty() {
            return true;
        }
        if file.seek(SeekFrom::Start(self.anchor_at)).is_err() {
            return true;
        }
        let mut buffer = vec![0u8; self.anchor.len()];
        if file.read_exact(&mut buffer).is_err() {
            return false;
        }
        buffer == self.anchor
    }

    /// Removes every complete line from `pending`, plus an over-long one.
    fn take_lines(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        let mut start = 0;
        while let Some(index) = self.pending[start..].iter().position(|byte| *byte == b'\n') {
            let end = start + index;
            lines.push(decode(&self.pending[start..end]));
            start = end + 1;
        }
        if start > 0 {
            self.pending.drain(..start);
        }
        if self.pending.len() > PENDING_LIMIT {
            lines.push(decode(&std::mem::take(&mut self.pending)));
        }
        lines
    }
}

/// Where [`Follower::new`] starts reading. See [`SMALL`] and [`FRESH`].
fn start_position(metadata: &std::fs::Metadata) -> u64 {
    let fresh = metadata
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < FRESH);
    if fresh && metadata.len() <= SMALL {
        0
    } else {
        metadata.len()
    }
}

fn decode(line: &[u8]) -> String {
    String::from_utf8_lossy(line)
        .trim_end_matches('\r')
        .to_string()
}

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
    use crate::testutil::TempDir;
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

    // ---- follower ----------------------------------------------------------

    fn append(path: &Path, text: &str) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn appends_are_yielded_line_by_line() {
        let dir = TempDir::new("follow-lines");
        let path = dir.write("log.txt", "first\n");
        let mut follower = Follower::new(path.clone());
        // A small fresh file is read from the beginning: the line that arrived
        // before the follower started is not lost.
        assert_eq!(follower.poll(), ["first"]);
        assert!(follower.poll().is_empty(), "nothing new, nothing repeated");

        append(&path, "second\r\nthird\n");
        assert_eq!(follower.poll(), ["second", "third"]);
    }

    #[test]
    fn an_incomplete_line_waits_for_its_break() {
        let dir = TempDir::new("follow-partial");
        let path = dir.write("log.txt", "");
        let mut follower = Follower::new(path.clone());

        append(&path, "half");
        assert!(follower.poll().is_empty());
        append(&path, " a line\nand another");
        assert_eq!(follower.poll(), ["half a line"]);
        append(&path, "\n");
        assert_eq!(follower.poll(), ["and another"]);
    }

    #[test]
    fn a_file_that_does_not_exist_yet_is_picked_up_when_it_appears() {
        let dir = TempDir::new("follow-late");
        let path = dir.path().join("log_cl.txt");
        let mut follower = Follower::new(path.clone());
        assert!(follower.poll().is_empty());

        std::fs::write(&path, "late\n").unwrap();
        assert_eq!(follower.poll(), ["late"]);
    }

    #[test]
    fn a_truncated_file_is_read_from_the_beginning_again() {
        let dir = TempDir::new("follow-truncate");
        // Big enough to be followed from its end, like the log of a run that
        // is over.
        let path = dir.write(
            "log.txt",
            &format!("old run\n{}", "x".repeat(SMALL as usize)),
        );
        let mut follower = Follower::new(path.clone());
        assert!(follower.poll().is_empty());

        // Rotation in place: the same file starts over, shorter than what the
        // follower had already read.
        std::fs::write(&path, "===== Parsec: Started =====\n").unwrap();
        assert_eq!(follower.poll(), ["===== Parsec: Started ====="]);
    }

    #[test]
    fn a_large_file_is_followed_from_its_end_only() {
        // A previous run's big log is not dumped into the console; only what
        // is appended from now on is shown.
        let dir = TempDir::new("follow-large");
        let path = dir.write("log.txt", &format!("{}\n", "x".repeat(SMALL as usize)));
        let mut follower = Follower::new(path.clone());
        assert!(follower.poll().is_empty());

        append(&path, "new line\n");
        assert_eq!(follower.poll(), ["new line"]);
    }

    #[test]
    fn a_replaced_file_is_read_from_the_beginning_again() {
        let dir = TempDir::new("follow-replace");
        let path = dir.write("log.txt", "old run\n");
        let mut follower = Follower::new(path.clone());
        assert_eq!(follower.poll(), ["old run"]);

        // Rotation by rename and a fresh file, which is at least as large as
        // what the old one had; only the creation time gives it away.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "another run with more bytes\n").unwrap();
        assert_eq!(follower.poll(), ["another run with more bytes"]);
    }

    #[test]
    fn a_line_without_a_break_cannot_grow_without_bound() {
        let dir = TempDir::new("follow-huge");
        let path = dir.write("log.txt", "");
        let mut follower = Follower::new(path.clone());

        append(&path, &"y".repeat(PENDING_LIMIT + 1));
        let lines = follower.poll();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), PENDING_LIMIT + 1);
    }

    #[test]
    fn non_utf8_bytes_do_not_panic() {
        let dir = TempDir::new("follow-bytes");
        let path = dir.write("log.txt", "");
        let mut follower = Follower::new(path.clone());

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(&[0xFF, 0xFE, b'\n']).unwrap();
        let lines = follower.poll();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains('\u{FFFD}'));
    }
}
