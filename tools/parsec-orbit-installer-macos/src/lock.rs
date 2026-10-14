//! Serialize installers selecting the same app, even with different state roots.
//! The sidecar inode is kept after unlock: unlinking it would allow two locks.

use std::fs::File;
use std::path::Path;

pub struct AppLock {
    _file: File,
}

#[cfg(unix)]
impl Drop for AppLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // Other test/controller threads may fork while this fd is live.
        // Explicit unlock releases the shared open-file description even
        // before their CLOEXEC descriptors are closed by exec.
        unsafe {
            libc::flock(self._file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl AppLock {
    #[cfg(unix)]
    pub fn acquire(app: &Path) -> Result<Self, String> {
        use sha2::{Digest, Sha256};
        use std::fs::OpenOptions;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        let app = crate::platform::canonical_absolute(app)?;
        let parent = app.parent().ok_or("selected app has no parent")?;
        let name = app.file_name().ok_or("selected app has no file name")?;
        let key = crate::digest::hex(&Sha256::digest(name.as_encoded_bytes()));
        let path = parent.join(format!(".parsec-orbit-{key}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&path)
            .or_else(|error| {
                #[cfg(target_os = "macos")]
                if error.kind() == std::io::ErrorKind::PermissionDenied {
                    return crate::privilege::create_lock(&path);
                }
                Err(error)
            })
            .map_err(|error| format!("cannot lock {}: {error}", app.display()))?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || metadata.uid() != crate::platform::effective_uid()
            || metadata.nlink() != 1
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(format!(
                "{} is not a private owned lock file",
                path.display()
            ));
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!(
                "another installer holds {}: {}",
                app.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self { _file: file })
    }

    #[cfg(not(unix))]
    pub fn acquire(_app: &Path) -> Result<Self, String> {
        Err("installer locking requires a Unix host".into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use parsec_orbit_core::testutil::TempDir;

    #[test]
    fn serializes_same_path_and_releases_on_drop() {
        let scratch = TempDir::new("installer-lock");
        let app = scratch.path().join("Parsec.app");
        let first = AppLock::acquire(&app).unwrap();
        assert!(AppLock::acquire(&app).is_err());
        let other = AppLock::acquire(&scratch.path().join("Other.app")).unwrap();
        drop(first);
        assert!(AppLock::acquire(&app).is_ok());
        drop(other);
    }
}
