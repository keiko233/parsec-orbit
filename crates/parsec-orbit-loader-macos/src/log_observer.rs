//! Bounded observation of one private log for current-launch readiness.
//!
//! Construct [`LogObserver`] before launching the client. The observer starts
//! at the existing file length (or zero when it is absent), so bytes left by
//! an earlier launch are never returned. It returns raw bytes: callers must
//! decode UTF-8 incrementally and must not treat an incomplete final line as
//! a complete record. Each call performs synchronous regular-file I/O; the
//! byte bounds do not provide a hard asynchronous deadline.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Maximum bytes read into the retained buffer by one observation.
pub const MAX_READ_BYTES: usize = 4096;
/// Maximum current-launch bytes retained by one observer.
pub const MAX_RETAINED_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

/// A pre-launch cursor over one caller-selected log in a validated private
/// state root. Every error permanently poisons the cursor.
pub struct LogObserver {
    root: PathBuf,
    directory: File,
    basename: std::ffi::CString,
    path: PathBuf,
    uid: u32,
    offset: u64,
    identity: Option<Identity>,
    retained: Vec<u8>,
    failed: bool,
}

impl LogObserver {
    /// Snapshots a log before launch. `state_root` must already have been
    /// selected and validated by the caller. The root and any existing log
    /// are checked again during every observation.
    pub fn snapshot(state_root: &Path, basename: &str, uid: u32) -> Result<Self, String> {
        if !safe_basename(basename) {
            return Err("log basename is not a single safe filename".into());
        }
        let initial = validate_root(state_root, uid)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(state_root)
            .map_err(|error| format!("cannot pin private log directory: {error}"))?;
        let opened = directory.metadata().map_err(|error| error.to_string())?;
        if initial.dev() != opened.dev() || initial.ino() != opened.ino() {
            return Err("private log directory changed while opening".into());
        }
        let path = state_root.join(basename);
        let basename = std::ffi::CString::new(basename).expect("validated ASCII basename");
        let (offset, identity) = match open_log(&directory, &basename, uid) {
            Ok((_file, metadata)) => (
                metadata.len(),
                Some(Identity {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                }),
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (0, None),
            Err(error) => return Err(format!("cannot snapshot log {}: {error}", path.display())),
        };

        Ok(Self {
            root: state_root.to_path_buf(),
            directory,
            basename,
            path,
            uid,
            offset,
            identity,
            retained: Vec::new(),
            failed: false,
        })
    }

    /// Reads at most [`MAX_READ_BYTES`] newly appended bytes and returns all
    /// current-launch bytes retained so far. Call repeatedly to consume a
    /// larger append. The returned slice remains valid until the next call.
    pub fn read_current_launch(&mut self) -> Result<&[u8], String> {
        if self.failed {
            return Err("log observer has permanently failed".into());
        }
        match self.read_once() {
            Ok(()) => Ok(&self.retained),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    /// Returns only complete UTF-8 records ending with a newline. Incomplete
    /// trailing records remain buffered. Invalid complete UTF-8 fails closed.
    pub fn read_complete_lines(&mut self) -> Result<&str, String> {
        self.read_current_launch()?;
        let end = self
            .retained
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        match std::str::from_utf8(&self.retained[..end]) {
            Ok(text) => Ok(text),
            Err(error) => {
                self.failed = true;
                Err(format!("invalid UTF-8 in complete log records: {error}"))
            }
        }
    }

    /// Connects complete current-launch log records to the readiness policy.
    /// `mapped_images` must be supplied by a separate runtime image observer.
    /// This call performs synchronous I/O and is not a nonblocking deadline probe.
    pub fn observe_client_state(
        &mut self,
        mapped_images: &[PathBuf],
    ) -> crate::readiness::ProbeState {
        let state = self.root.clone();
        match self.read_complete_lines() {
            Ok(lines) => crate::readiness::client_started(&state, mapped_images, lines),
            Err(error) => crate::readiness::ProbeState::Failed(error),
        }
    }

    fn read_once(&mut self) -> Result<(), String> {
        let root = validate_root(&self.root, self.uid)?;
        let pinned = self
            .directory
            .metadata()
            .map_err(|error| error.to_string())?;
        if root.dev() != pinned.dev() || root.ino() != pinned.ino() {
            return Err("private log directory identity changed".into());
        }
        let (file, metadata) = match open_log(&self.directory, &self.basename, self.uid) {
            Ok(opened) => opened,
            Err(error) if error.kind() == io::ErrorKind::NotFound && self.identity.is_none() => {
                return Ok(());
            }
            Err(error) => {
                return Err(format!(
                    "cannot safely open log {}: {error}",
                    self.path.display()
                ));
            }
        };

        let current = Identity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        match self.identity {
            Some(expected) if current != expected => {
                return Err("log inode changed after snapshot".into());
            }
            None => {
                // The file was absent before launch. Its first appearance
                // establishes the inode for all following observations.
                self.identity = Some(current);
            }
            _ => {}
        }
        if metadata.len() < self.offset {
            return Err("log was truncated after snapshot".into());
        }

        let available = metadata.len() - self.offset;
        let remaining = MAX_RETAINED_BYTES.saturating_sub(self.retained.len());
        if available > remaining as u64 {
            return Err(format!(
                "current-launch log data exceeds {MAX_RETAINED_BYTES} retained bytes"
            ));
        }
        let amount = available.min(MAX_READ_BYTES as u64) as usize;
        if amount == 0 {
            validate_path_identity(&self.directory, &self.basename, current, self.uid)?;
            return Ok(());
        }

        let old_len = self.retained.len();
        self.retained.resize(old_len + amount, 0);
        let mut read = 0;
        while read < amount {
            match file.read_at(
                &mut self.retained[old_len + read..],
                self.offset + read as u64,
            ) {
                Ok(0) => {
                    self.retained.truncate(old_len);
                    return Err("log changed while reading appended bytes".into());
                }
                Ok(count) => read += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    self.retained.truncate(old_len);
                    return Err(format!("cannot read appended log bytes: {error}"));
                }
            }
        }

        let after = file
            .metadata()
            .map_err(|error| format!("cannot recheck opened log: {error}"))?;
        if after.dev() != current.device
            || after.ino() != current.inode
            || after.len() < self.offset + amount as u64
        {
            self.retained.truncate(old_len);
            return Err("log identity or length changed while reading".into());
        }
        validate_path_identity(&self.directory, &self.basename, current, self.uid)?;
        self.offset += amount as u64;
        Ok(())
    }
}

fn safe_basename(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_root(root: &Path, uid: u32) -> Result<fs::Metadata, String> {
    let metadata = fs::symlink_metadata(root).map_err(|error| {
        format!(
            "cannot inspect private state root {}: {error}",
            root.display()
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
    {
        return Err(format!(
            "state root {} must be a current-user-owned private directory",
            root.display()
        ));
    }
    let canonical = fs::canonicalize(root)
        .map_err(|error| format!("cannot resolve state root {}: {error}", root.display()))?;
    if canonical != root {
        return Err(format!("state root {} is not canonical", root.display()));
    }
    Ok(metadata)
}

fn open_log(
    directory: &File,
    basename: &std::ffi::CStr,
    uid: u32,
) -> io::Result<(File, fs::Metadata)> {
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            basename.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "log is not a regular file",
        ));
    }
    // The directory was validated and pinned as a current-user-owned 0700
    // root, so other users cannot traverse to this file. The measured vendor
    // log is created 0644; requiring owner-only bits on the file itself
    // rejected every real launch. Group/other write stays refused.
    if metadata.uid() != uid || metadata.mode() & 0o022 != 0 || metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "log must be current-user-owned and not writable by group or other",
        ));
    }
    Ok((file, metadata))
}

fn validate_path_identity(
    directory: &File,
    basename: &std::ffi::CStr,
    expected: Identity,
    uid: u32,
) -> Result<(), String> {
    let (_, metadata) = open_log(directory, basename, uid)
        .map_err(|error| format!("cannot revalidate pinned log path: {error}"))?;
    if metadata.dev() != expected.device || metadata.ino() != expected.inode {
        return Err("log inode changed during observation".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "parsec-orbit-log-observer-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }

        fn log(&self) -> PathBuf {
            self.0.join("orbit.log")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn uid() -> u32 {
        unsafe { libc::getuid() }
    }

    fn create_log(path: &Path, mode: u32, bytes: &[u8]) {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(mode);
        let mut file = options.open(path).unwrap();
        file.write_all(bytes).unwrap();
    }

    fn replace_log(scratch: &Scratch, bytes: &[u8]) {
        let replacement = scratch.0.join("replacement.log");
        create_log(&replacement, 0o600, bytes);
        fs::rename(replacement, scratch.log()).unwrap();
    }

    #[test]
    fn existing_log_baselines_old_release_and_returns_only_append() {
        let scratch = Scratch::new();
        create_log(
            &scratch.log(),
            0o600,
            b"Parsec release (150-101a, Loader: 14)\n",
        );
        let mut observer = LogObserver::snapshot(&scratch.0, "orbit.log", uid()).unwrap();
        assert_eq!(observer.read_current_launch().unwrap(), b"");
        OpenOptions::new()
            .append(true)
            .open(scratch.log())
            .unwrap()
            .write_all(b"current launch\n")
            .unwrap();
        assert_eq!(observer.read_current_launch().unwrap(), b"current launch\n");
        assert!(
            !std::str::from_utf8(observer.read_current_launch().unwrap())
                .unwrap()
                .contains("Parsec release")
        );
    }

    #[test]
    fn absent_log_accepts_first_creation_and_split_appends() {
        let scratch = Scratch::new();
        let mut observer = LogObserver::snapshot(&scratch.0, "orbit.log", uid()).unwrap();
        assert_eq!(observer.read_current_launch().unwrap(), b"");
        create_log(&scratch.log(), 0o600, b"Parsec release (150-");
        assert_eq!(
            observer.read_current_launch().unwrap(),
            b"Parsec release (150-"
        );
        OpenOptions::new()
            .append(true)
            .open(scratch.log())
            .unwrap()
            .write_all(b"101a, Loader: 14)\n")
            .unwrap();
        assert_eq!(
            observer.read_current_launch().unwrap(),
            b"Parsec release (150-101a, Loader: 14)\n"
        );
    }

    #[test]
    fn read_per_call_is_bounded_and_retained_total_is_bounded() {
        let scratch = Scratch::new();
        let mut observer = LogObserver::snapshot(&scratch.0, "orbit.log", uid()).unwrap();
        create_log(&scratch.log(), 0o600, &vec![b'x'; MAX_READ_BYTES + 8]);
        assert_eq!(
            observer.read_current_launch().unwrap().len(),
            MAX_READ_BYTES
        );
        assert_eq!(
            observer.read_current_launch().unwrap().len(),
            MAX_READ_BYTES + 8
        );

        let second = Scratch::new();
        let mut observer = LogObserver::snapshot(&second.0, "orbit.log", uid()).unwrap();
        create_log(&second.log(), 0o600, &vec![b'x'; MAX_RETAINED_BYTES + 1]);
        assert!(observer.read_current_launch().is_err());
        assert!(observer.read_current_launch().is_err());
    }

    #[test]
    fn inode_replacement_and_truncation_permanently_fail() {
        let scratch = Scratch::new();
        create_log(&scratch.log(), 0o600, b"before");
        let mut observer = LogObserver::snapshot(&scratch.0, "orbit.log", uid()).unwrap();
        replace_log(&scratch, b"replacement");
        assert!(observer.read_current_launch().is_err());

        let third = Scratch::new();
        let mut observer = LogObserver::snapshot(&third.0, "orbit.log", uid()).unwrap();
        create_log(&third.log(), 0o600, b"first-created");
        assert_eq!(observer.read_current_launch().unwrap(), b"first-created");
        replace_log(&third, b"replaced-after-create");
        assert!(observer.read_current_launch().is_err());
        assert!(observer.read_current_launch().is_err());

        let second = Scratch::new();
        create_log(&second.log(), 0o600, b"baseline-long");
        let mut observer = LogObserver::snapshot(&second.0, "orbit.log", uid()).unwrap();
        OpenOptions::new()
            .write(true)
            .open(second.log())
            .unwrap()
            .set_len(0)
            .unwrap();
        assert!(observer.read_current_launch().is_err());
    }

    #[test]
    fn symlink_dangling_symlink_fifo_and_unsafe_permissions_fail() {
        let scratch = Scratch::new();
        symlink("missing-target", scratch.log()).unwrap();
        assert!(LogObserver::snapshot(&scratch.0, "orbit.log", uid()).is_err());

        fs::remove_file(scratch.log()).unwrap();
        let fifo = scratch.log();
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert!(LogObserver::snapshot(&scratch.0, "orbit.log", uid()).is_err());

        fs::remove_file(scratch.log()).unwrap();
        create_log(&scratch.log(), 0o600, b"group writable");
        // The process umask strips group/other write from a created mode, so
        // force the unsafe bits back before the check.
        fs::set_permissions(scratch.log(), fs::Permissions::from_mode(0o666)).unwrap();
        assert!(LogObserver::snapshot(&scratch.0, "orbit.log", uid()).is_err());
        fs::remove_file(scratch.log()).unwrap();
        // The measured vendor log is 0644 inside the validated 0700 root and
        // must be observable; only group/other write is refused.
        create_log(&scratch.log(), 0o644, b"vendor shaped");
        assert!(LogObserver::snapshot(&scratch.0, "orbit.log", uid()).is_ok());

        let second = Scratch::new();
        let mut observer = LogObserver::snapshot(&second.0, "orbit.log", uid()).unwrap();
        symlink("missing-target", second.log()).unwrap();
        assert!(observer.read_current_launch().is_err());
        assert!(observer.read_current_launch().is_err());

        let third = Scratch::new();
        let mut observer = LogObserver::snapshot(&third.0, "orbit.log", uid()).unwrap();
        let fifo = third.log();
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert!(observer.read_current_launch().is_err());

        let fourth = Scratch::new();
        create_log(&fourth.log(), 0o600, b"safe at snapshot");
        let mut observer = LogObserver::snapshot(&fourth.0, "orbit.log", uid()).unwrap();
        fs::set_permissions(fourth.log(), fs::Permissions::from_mode(0o660)).unwrap();
        assert!(observer.read_current_launch().is_err());
    }

    #[test]
    fn rejects_unsafe_basenames_and_nonprivate_roots() {
        let scratch = Scratch::new();
        assert!(LogObserver::snapshot(&scratch.0, "../orbit.log", uid()).is_err());
        assert!(LogObserver::snapshot(&scratch.0, "sub/orbit.log", uid()).is_err());
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(LogObserver::snapshot(&scratch.0, "orbit.log", uid()).is_err());
    }

    #[test]
    fn readiness_uses_only_new_complete_records() {
        use crate::readiness::ProbeState;
        let scratch = Scratch::new();
        create_log(
            &scratch.log(),
            0o600,
            b"Parsec release (150-101a, Loader: 14)\n",
        );
        let mut observer = LogObserver::snapshot(&scratch.0, "orbit.log", uid()).unwrap();
        let images = vec![scratch.0.join("parsecd-150-101a.dylib")];
        assert!(matches!(
            observer.observe_client_state(&images),
            ProbeState::NotYet(_)
        ));
        let mut writer = OpenOptions::new().append(true).open(scratch.log()).unwrap();
        writer
            .write_all(b"Parsec release (150-101a, Loader: 14)")
            .unwrap();
        assert!(matches!(
            observer.observe_client_state(&images),
            ProbeState::NotYet(_)
        ));
        writer.write_all(b"\n").unwrap();
        assert!(matches!(
            observer.observe_client_state(&images),
            ProbeState::Ready(_)
        ));
    }

    #[test]
    fn split_utf8_is_buffered_and_invalid_complete_utf8_fails() {
        let scratch = Scratch::new();
        let mut observer = LogObserver::snapshot(&scratch.0, "orbit.log", uid()).unwrap();
        create_log(&scratch.log(), 0o600, b"prefix \xe2\x82");
        assert_eq!(observer.read_complete_lines().unwrap(), "");
        let mut writer = OpenOptions::new().append(true).open(scratch.log()).unwrap();
        writer.write_all(b"\xac\n").unwrap();
        assert_eq!(observer.read_complete_lines().unwrap(), "prefix \u{20ac}\n");
        writer.write_all(b"\xff\n").unwrap();
        assert!(observer.read_complete_lines().is_err());
        assert!(observer.read_current_launch().is_err());
    }

    #[test]
    fn hardlinks_and_directory_replacement_fail_before_log_consumption() {
        let scratch = Scratch::new();
        create_log(&scratch.log(), 0o600, b"old");
        fs::hard_link(scratch.log(), scratch.0.join("alias")).unwrap();
        assert!(LogObserver::snapshot(&scratch.0, "orbit.log", uid()).is_err());
        fs::remove_file(scratch.0.join("alias")).unwrap();
        let mut observer = LogObserver::snapshot(&scratch.0, "orbit.log", uid()).unwrap();
        fs::hard_link(scratch.log(), scratch.0.join("alias")).unwrap();
        assert!(observer.read_current_launch().is_err());

        let state = scratch.0.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        create_log(&state.join("orbit.log"), 0o600, b"baseline");
        let mut observer = LogObserver::snapshot(&state, "orbit.log", uid()).unwrap();
        fs::rename(&state, scratch.0.join("old-state")).unwrap();
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        create_log(
            &state.join("orbit.log"),
            0o600,
            b"Parsec release (150-101a, Loader: 14)\n",
        );
        assert!(observer.read_current_launch().is_err());
        assert!(observer.read_current_launch().is_err());
    }
}
