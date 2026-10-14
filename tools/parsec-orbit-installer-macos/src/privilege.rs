//! Explicit sudo mode: keep user-owned state and tool children unprivileged.
//! Only protected bundle renames and creation of the per-app lock may
//! temporarily regain the root identity retained by the CLI process.

use std::io;
use std::path::Path;

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    struct Context {
        uid: u32,
        gid: u32,
        app: PathBuf,
        app_parent: File,
        orbit_root: PathBuf,
        orbit_parent: OnceLock<File>,
    }
    static CONTEXT: OnceLock<Context> = OnceLock::new();

    pub fn enter(uid: u32, app: &Path, root: &Path) -> Result<(), String> {
        if uid == 0 || unsafe { libc::getuid() != 0 || libc::geteuid() != 0 } {
            return Err("--sudo-user needs a root invocation and a non-root user UID".into());
        }
        if let Ok(caller) = std::env::var("SUDO_UID") {
            if caller.parse::<u32>().ok() != Some(uid) {
                return Err("--sudo-user must match the invoking SUDO_UID".into());
            }
        }
        let app = crate::platform::canonical_absolute(app)?;
        let orbit_root = crate::platform::canonical_absolute(root)?;
        let app_parent = open_directory(app.parent().ok_or("app has no parent")?)
            .map_err(|error| format!("cannot pin selected app parent: {error}"))?;
        if app == orbit_root || app.starts_with(&orbit_root) || orbit_root.starts_with(&app) {
            return Err("the selected bundle and Orbit state must be separate trees".into());
        }
        let mut storage = vec![0_u8; 65536];
        let mut password: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found = std::ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                &mut password,
                storage.as_mut_ptr().cast(),
                storage.len(),
                &mut found,
            )
        };
        if status != 0 || found.is_null() {
            return Err(format!("cannot resolve target user UID {uid}"));
        }
        let name = unsafe { CStr::from_ptr(password.pw_name) }.to_owned();
        let home = unsafe { CStr::from_ptr(password.pw_dir) }.to_owned();
        let gid = password.pw_gid;
        let group =
            i32::try_from(gid).map_err(|_| "target primary group exceeds supported range")?;
        if CONTEXT.get().is_some() {
            return Err("sudo identity was already selected".into());
        }
        // The CLI is single-threaded. No library or worker is started before
        // this process-wide identity transition. Supplementary groups also
        // belong to the target user, not the administrator's root identity.
        if unsafe { libc::initgroups(name.as_ptr(), group) } != 0
            || unsafe { libc::setgid(gid) } != 0
            || unsafe { libc::seteuid(uid) } != 0
        {
            return Err(format!(
                "cannot select ordinary user: {}",
                io::Error::last_os_error()
            ));
        }
        CONTEXT
            .set(Context {
                uid,
                gid,
                app,
                app_parent,
                orbit_root,
                orbit_parent: OnceLock::new(),
            })
            .map_err(|_| "sudo identity was already selected".to_string())?;
        std::env::set_var("HOME", std::ffi::OsStr::from_bytes(home.to_bytes()));
        // Uninstall and interrupted recovery use an existing state root
        // before the apply backup step, so pin it at identity selection too.
        let selected = &CONTEXT.get().expect("context just set").orbit_root;
        if selected.exists() {
            crate::platform::require_private_dir(selected, "Orbit root")?;
        }
        Ok(())
    }

    pub fn child(command: &mut std::process::Command) {
        if let Some(context) = CONTEXT.get() {
            let (uid, gid) = (context.uid, context.gid);
            // SAFETY: only async-signal-safe identity syscalls run after fork.
            // Regain root in this child solely to discard ALL real/effective/
            // saved IDs before exec; Command::uid alone while euid is dropped
            // would not necessarily discard the retained root real UID.
            unsafe {
                command.pre_exec(move || {
                    if libc::seteuid(0) != 0 || libc::setgid(gid) != 0 || libc::setuid(uid) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::getuid() != uid || libc::geteuid() != uid {
                        return Err(io::Error::from_raw_os_error(libc::EPERM));
                    }
                    Ok(())
                });
            }
        }
    }

    pub fn enabled() -> bool {
        CONTEXT.get().is_some()
    }

    fn as_root<T>(operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let context = CONTEXT
            .get()
            .ok_or_else(|| io::Error::other("no sudo context"))?;
        if unsafe { libc::seteuid(0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let result = operation();
        if unsafe { libc::seteuid(context.uid) } != 0 {
            // Continuing with unexpected privilege is never acceptable.
            unsafe { libc::_exit(79) };
        }
        result
    }

    fn open_at(parent: &File, name: &std::ffi::OsStr) -> io::Result<File> {
        let name = CString::new(name.as_bytes())?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }

    fn open_directory(path: &Path) -> io::Result<File> {
        let fd = unsafe {
            libc::open(
                c"/".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        for component in path.components() {
            match component {
                std::path::Component::RootDir => {}
                std::path::Component::Normal(name) => file = open_at(&file, name)?,
                _ => {
                    return Err(io::Error::other(
                        "directory path must be absolute and canonical",
                    ))
                }
            }
        }
        Ok(file)
    }

    fn private_directory(file: &File, uid: u32) -> io::Result<()> {
        let metadata = file.metadata()?;
        if metadata.uid() != uid || metadata.mode() & 0o7777 != 0o700 {
            return Err(io::Error::other(
                "transaction directory is not private and user-owned",
            ));
        }
        Ok(())
    }

    pub fn pin_root(path: &Path) -> io::Result<()> {
        let Some(context) = CONTEXT.get() else {
            return Ok(());
        };
        if path != context.orbit_root || context.orbit_parent.get().is_some() {
            return Ok(());
        }
        let file = open_directory(path)?;
        private_directory(&file, context.uid)?;
        context
            .orbit_parent
            .set(file)
            .map_err(|_| io::Error::other("state root already pinned"))
    }

    fn install_parent(path: &Path, context: &Context) -> io::Result<File> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("missing transaction parent"))?;
        let installs = context.orbit_root.join("installs");
        let relative = parent
            .strip_prefix(&installs)
            .map_err(|_| io::Error::other("rename outside transaction"))?;
        let mut components = relative.components();
        let Some(std::path::Component::Normal(id)) = components.next() else {
            return Err(io::Error::other("missing transaction id"));
        };
        if components.next().is_some() {
            return Err(io::Error::other("nested transaction parent"));
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::other("missing member name"))?;
        if !(name == "pristine"
            || name == "staged"
            || name == "applied-removed"
            || name.strip_prefix("applied-removed-").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
            }))
        {
            return Err(io::Error::other("unsupported transaction member"));
        }
        let root = context
            .orbit_parent
            .get()
            .ok_or_else(|| io::Error::other("state root was not pinned"))?;
        let installs = open_at(root, std::ffi::OsStr::new("installs"))?;
        private_directory(&installs, context.uid)?;
        let member = open_at(&installs, id)?;
        private_directory(&member, context.uid)?;
        Ok(member)
    }

    fn rename_relative(
        from: &File,
        source: &Path,
        to: &File,
        destination: &Path,
    ) -> io::Result<()> {
        let source = CString::new(
            source
                .file_name()
                .ok_or_else(|| io::Error::other("missing source name"))?
                .as_bytes(),
        )?;
        let destination = CString::new(
            destination
                .file_name()
                .ok_or_else(|| io::Error::other("missing destination name"))?
                .as_bytes(),
        )?;
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                from.as_raw_fd(),
                source.as_ptr(),
                &mut metadata,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if metadata.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(io::Error::other(
                "rename source is not a real bundle directory",
            ));
        }
        if unsafe {
            libc::renameatx_np(
                from.as_raw_fd(),
                source.as_ptr(),
                to.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_EXCL,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn rename_once(source: &Path, destination: &Path) -> io::Result<()> {
        let source = CString::new(source.as_os_str().as_bytes())?;
        let destination = CString::new(destination.as_os_str().as_bytes())?;
        // Atomic no-clobber applies to both ordinary and elevated renames.
        if unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) }
            == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn rename(source: &Path, destination: &Path) -> io::Result<()> {
        let Some(context) = CONTEXT.get() else {
            return rename_once(source, destination);
        };
        let private = if source == context.app {
            install_parent(destination, context)?
        } else if destination == context.app {
            install_parent(source, context)?
        } else {
            return Err(io::Error::other(
                "sudo rename outside selected bundle transaction",
            ));
        };
        let (from, to) = if source == context.app {
            (&context.app_parent, &private)
        } else {
            (&private, &context.app_parent)
        };
        // Pin both parents before checking or elevating. No path is resolved
        // again under root, including after a same-user parent rename.
        match rename_relative(from, source, to, destination) {
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                as_root(|| rename_relative(from, source, to, destination))
            }
            result => result,
        }
    }

    pub fn create_lock(path: &Path) -> io::Result<std::fs::File> {
        let context = CONTEXT
            .get()
            .ok_or_else(|| io::Error::other("no sudo context"))?;
        let name = context
            .app
            .file_name()
            .ok_or_else(|| io::Error::other("no app name"))?;
        use sha2::{Digest, Sha256};
        let expected = context.app.parent().unwrap().join(format!(
            ".parsec-orbit-{}.lock",
            crate::digest::hex(&Sha256::digest(name.as_encoded_bytes()))
        ));
        if path != expected {
            return Err(io::Error::other("lock path outside selected app"));
        }
        as_root(|| {
            let name = CString::new(path.file_name().unwrap().as_bytes())?;
            let fd = unsafe {
                libc::openat(
                    context.app_parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let file = unsafe { File::from_raw_fd(fd) };
            if unsafe { libc::fchown(file.as_raw_fd(), context.uid, context.gid) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(file)
        })
    }
    #[cfg(test)]
    mod pinned_tests {
        use super::*;
        use parsec_orbit_core::testutil::TempDir;

        #[test]
        fn pinned_parent_rename_ignores_a_replacement_symlink() {
            let scratch = TempDir::new("installer-pinned-rename");
            let root = std::fs::canonicalize(scratch.path()).unwrap();
            let source_parent = root.join("source");
            let destination_parent = root.join("transaction");
            let moved_parent = root.join("moved-transaction");
            let unrelated = root.join("unrelated");
            for path in [&source_parent, &destination_parent, &unrelated] {
                std::fs::create_dir(path).unwrap();
            }
            let source = source_parent.join("Parsec.app");
            std::fs::create_dir(&source).unwrap();
            std::fs::write(source.join("payload"), b"pristine").unwrap();
            let source_fd = open_directory(&source_parent).unwrap();
            let destination_fd = open_directory(&destination_parent).unwrap();
            std::fs::rename(&destination_parent, &moved_parent).unwrap();
            std::os::unix::fs::symlink(&unrelated, &destination_parent).unwrap();
            rename_relative(
                &source_fd,
                &source,
                &destination_fd,
                &destination_parent.join("pristine"),
            )
            .unwrap();
            assert!(moved_parent.join("pristine/payload").is_file());
            assert!(!unrelated.join("pristine").exists());
            assert!(open_directory(&destination_parent).is_err());
        }
    }
}

/// Select a non-root sudo caller before transaction or tool execution.
pub fn enter(uid: u32, app: &Path, root: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        macos::enter(uid, app, root)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (uid, app, root);
        Err("--sudo-user is supported only on macOS".into())
    }
}

/// Make external tools discard retained root IDs before exec.
#[cfg(target_os = "macos")]
pub fn child(command: &mut std::process::Command) {
    macos::child(command);
}

/// Transaction bundle rename, no-clobber and scoped elevation on macOS.
pub fn rename(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::rename(source, destination)
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::rename(source, destination)
    }
}

/// Create only the selected app's lock when its parent is protected.
#[cfg(target_os = "macos")]
pub fn create_lock(path: &Path) -> io::Result<std::fs::File> {
    macos::create_lock(path)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use parsec_orbit_core::testutil::TempDir;

    #[test]
    fn bundle_rename_never_overwrites_an_occupied_destination() {
        let root = TempDir::new("installer-no-clobber-rename");
        let original = root.path().join("original.app");
        let occupied = root.path().join("occupied.app");
        std::fs::create_dir(&original).unwrap();
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(original.join("payload"), b"pristine").unwrap();
        std::fs::write(occupied.join("payload"), b"replacement").unwrap();
        assert!(rename(&original, &occupied).is_err());
        assert_eq!(
            std::fs::read(original.join("payload")).unwrap(),
            b"pristine"
        );
        assert_eq!(
            std::fs::read(occupied.join("payload")).unwrap(),
            b"replacement"
        );
        let free = root.path().join("free.app");
        rename(&original, &free).unwrap();
        assert_eq!(std::fs::read(free.join("payload")).unwrap(), b"pristine");
    }
}

#[cfg(target_os = "macos")]
pub fn pin_root(path: &Path) -> io::Result<()> {
    macos::pin_root(path)
}

pub fn enabled() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::enabled()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}
