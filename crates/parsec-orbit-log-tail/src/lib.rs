//! Incremental file following with rotation detection for log viewers.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How often a caller can poll followed files.
pub const FOLLOW_INTERVAL: Duration = Duration::from_millis(200);

/// A file that is at most this large and was written within [`FRESH`] is the
/// current run's freshly rotated log: its first lines were written before the
/// viewer started, so the beginning is shown too. Older or larger files are a
/// previous run and are followed from their end only.
const SMALL: u64 = 64 * 1024;
const FRESH: Duration = Duration::from_secs(15);

/// Maximum total bytes read by one call to [`Follower::poll`].
const READ_LIMIT: usize = 64 * 1024;

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
    /// line is kept until its line break shows up. Reads at most 64 KiB total
    /// per poll, including the bytes used to check for file replacement.
    pub fn poll(&mut self) -> Vec<String> {
        let Ok(metadata) = std::fs::metadata(&self.path) else {
            // Not created yet or gone again; either way there is nothing to read.
            return Vec::new();
        };

        if metadata.len() < self.position {
            // Truncated in place, typically the rotation of a fresh run.
            self.restart();
        }

        let read_limit = READ_LIMIT.saturating_sub(self.anchor.len());
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
        let remaining = (metadata.len() - self.position).min(read_limit as u64) as usize;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "parsec-orbit-log-tail-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create the temporary directory");
            TempDir(path)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }

        fn write(&self, name: &str, contents: &[u8]) -> PathBuf {
            let path = self.path(name);
            std::fs::write(&path, contents).expect("write the temporary file");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn append(path: &Path, bytes: &[u8]) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(bytes).unwrap();
    }

    #[test]
    fn appends_are_yielded_line_by_line() {
        let dir = TempDir::new();
        let path = dir.write("log.txt", b"first\n");
        let mut follower = Follower::new(path.clone());
        assert_eq!(follower.poll(), ["first"]);
        assert!(follower.poll().is_empty(), "nothing new, nothing repeated");

        append(&path, b"second\r\nthird\n");
        assert_eq!(follower.poll(), ["second", "third"]);
    }

    #[test]
    fn an_incomplete_line_waits_for_its_break() {
        let dir = TempDir::new();
        let path = dir.write("log.txt", b"");
        let mut follower = Follower::new(path.clone());

        append(&path, b"half");
        assert!(follower.poll().is_empty());
        append(&path, b" a line\nand another");
        assert_eq!(follower.poll(), ["half a line"]);
        append(&path, b"\n");
        assert_eq!(follower.poll(), ["and another"]);
    }

    #[test]
    fn a_file_that_does_not_exist_yet_is_picked_up_when_it_appears() {
        let dir = TempDir::new();
        let path = dir.path("log.txt");
        let mut follower = Follower::new(path.clone());
        assert!(follower.poll().is_empty());

        std::fs::write(&path, b"late\n").unwrap();
        assert_eq!(follower.poll(), ["late"]);
    }

    #[test]
    fn a_truncated_file_is_read_from_the_beginning_again() {
        let dir = TempDir::new();
        let path = dir.write(
            "log.txt",
            format!("old run\n{}", "x".repeat(SMALL as usize)).as_bytes(),
        );
        let mut follower = Follower::new(path.clone());
        assert!(follower.poll().is_empty());

        std::fs::write(&path, b"===== Parsec: Started =====\n").unwrap();
        assert_eq!(follower.poll(), ["===== Parsec: Started ====="]);
    }

    #[test]
    fn a_large_file_is_followed_from_its_end_only() {
        let dir = TempDir::new();
        let path = dir.write(
            "log.txt",
            format!("{}\n", "x".repeat(SMALL as usize)).as_bytes(),
        );
        let mut follower = Follower::new(path.clone());
        assert!(follower.poll().is_empty());

        append(&path, b"new line\n");
        assert_eq!(follower.poll(), ["new line"]);
    }

    #[test]
    fn a_replaced_file_is_read_from_the_beginning_again() {
        let dir = TempDir::new();
        let path = dir.write("log.txt", b"old run\n");
        let mut follower = Follower::new(path.clone());
        assert_eq!(follower.poll(), ["old run"]);

        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"another run with more bytes\n").unwrap();
        assert_eq!(follower.poll(), ["another run with more bytes"]);
    }

    #[test]
    fn a_line_without_a_break_cannot_grow_without_bound() {
        let dir = TempDir::new();
        let path = dir.write("log.txt", b"");
        let mut follower = Follower::new(path.clone());

        append(&path, &vec![b'y'; PENDING_LIMIT + 1]);
        assert!(follower.poll().is_empty());
        let lines = follower.poll();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), PENDING_LIMIT + 1);
    }

    #[test]
    fn non_utf8_bytes_do_not_panic() {
        let dir = TempDir::new();
        let path = dir.write("log.txt", b"");
        let mut follower = Follower::new(path.clone());

        append(&path, &[0xFF, 0xFE, b'\n']);
        let lines = follower.poll();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains('\u{FFFD}'));
    }

    #[test]
    fn large_appends_are_read_in_bounded_polls_until_caught_up() {
        let dir = TempDir::new();
        let path = dir.write("log.txt", b"");
        let mut follower = Follower::new(path.clone());
        let total_lines = READ_LIMIT / 5 * 3;
        append(&path, &b"line\n"[..].repeat(total_lines));

        let mut lines = Vec::new();
        while follower.position < (total_lines * 5) as u64 {
            let previous_position = follower.position;
            let anchor_bytes = follower.anchor.len();
            lines.extend(follower.poll());
            assert!(
                follower.position - previous_position + anchor_bytes as u64 <= READ_LIMIT as u64
            );
        }
        assert_eq!(lines.len(), total_lines);
        assert!(lines.iter().all(|line| line == "line"));
    }
}
