//! Host primitives: private-file helpers, the macOS tools and the process
//! identity check.
//!
//! The filesystem helpers are portable so the workspace check and the
//! portable tests run on every host. Everything that shells out to `codesign`,
//! `ditto`, `plutil` or `libproc` returns an error on non-macOS hosts; the
//! installer is a macOS tool and the Windows CI job only compiles and
//! unit-tests the portable modules.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use std::ffi::OsStr;
#[cfg(target_os = "macos")]
use std::process::Command;

/// The mode of a metadata entry, or 0 on hosts without POSIX modes.
#[cfg(unix)]
pub fn mode_bits(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    metadata.mode() & 0o7777
}

#[cfg(not(unix))]
pub fn mode_bits(_metadata: &fs::Metadata) -> u32 {
    0
}

/// The effective user id, or 0 on hosts without one.
#[cfg(unix)]
pub fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
pub fn effective_uid() -> u32 {
    0
}

/// Creates a new private (0700) directory. Fails if the path exists.
#[cfg(unix)]
pub fn create_private_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => require_private_dir(path, "directory"),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

#[cfg(not(unix))]
pub fn create_private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// Requires a real (not symlinked) directory that this user owns privately.
pub fn require_private_dir(path: &Path, label: &str) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(format!(
            "{label} {} is not a real directory",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != effective_uid() {
            return Err(format!(
                "{label} {} is not owned by the effective user",
                path.display()
            ));
        }
        if metadata.permissions().mode() & 0o7777 != 0o700 {
            return Err(format!("{label} {} must have mode 0700", path.display()));
        }
    }
    Ok(())
}

/// Creates a new private (0600) file, refusing an existing path.
#[cfg(unix)]
pub fn create_private_file(path: &Path) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(not(unix))]
pub fn create_private_file(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Writes a new private (0600) file, refusing an existing path.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut file = create_private_file(path)?;
    file.write_all(bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Opens (or creates) the append-only journal without following a symlink.
pub fn open_append_private(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Sets POSIX permission bits; a no-op elsewhere.
#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(not(unix))]
pub fn set_mode(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

/// Flushes a directory entry after a rename.
pub fn fsync_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let directory = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
        directory
            .sync_all()
            .map_err(|error| format!("{}: {error}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Canonicalizes a path whose final components may not exist yet: the
/// deepest existing ancestor is resolved (rejecting symlinked aliases in it)
/// and the missing components are appended unchanged.
pub fn canonical_absolute(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    let mut missing: Vec<OsString> = Vec::new();
    let mut current = path;
    loop {
        match fs::canonicalize(current) {
            Ok(canonical) => {
                let mut full = canonical;
                for component in missing.iter().rev() {
                    full.push(component);
                }
                return Ok(full);
            }
            Err(error) => {
                let Some(name) = current.file_name() else {
                    return Err(format!("{}: {error}", current.display()));
                };
                missing.push(name.to_os_string());
                match current.parent() {
                    Some(parent) => current = parent,
                    None => return Err(format!("{}: {error}", current.display())),
                }
            }
        }
    }
}

/// Whether two existing paths live on the same filesystem (needed for the
/// rename-based backup and swap).
#[cfg(unix)]
pub fn same_volume(left: &Path, right: &Path) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt;
    let left = fs::metadata(left).map_err(|error| format!("{}: {error}", left.display()))?;
    let right = fs::metadata(right).map_err(|error| format!("{}: {error}", right.display()))?;
    Ok(left.dev() == right.dev())
}

#[cfg(not(unix))]
pub fn same_volume(_left: &Path, _right: &Path) -> Result<bool, String> {
    Ok(true)
}

/// Whether this user may modify entries inside `directory`.
pub fn directory_writable(directory: &Path) -> Result<bool, String> {
    let path = std::ffi::CString::new(directory.as_os_str().as_encoded_bytes())
        .map_err(|_| format!("{} contains a NUL byte", directory.display()))?;
    #[cfg(unix)]
    {
        let writable = unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 };
        Ok(writable)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(true)
    }
}

#[cfg(target_os = "macos")]
fn tool(program: &str, arguments: &[&OsStr]) -> Result<String, String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = stderr
            .lines()
            .chain(stdout.lines())
            .find(|line| !line.trim().is_empty())
            .unwrap_or("no output");
        return Err(format!("{program} exited with {}: {detail}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(target_os = "macos")]
pub fn codesign_verify(bundle: &Path) -> Result<(), String> {
    tool(
        "codesign",
        &[
            OsStr::new("--verify"),
            OsStr::new("--strict"),
            bundle.as_os_str(),
        ],
    )
    .map(|_| ())
}

#[cfg(not(target_os = "macos"))]
pub fn codesign_verify(_bundle: &Path) -> Result<(), String> {
    Err("codesign verification requires macOS".into())
}

#[cfg(target_os = "macos")]
pub fn codesign_sign(
    path: &Path,
    entitlements: Option<&Path>,
    runtime: bool,
) -> Result<(), String> {
    let mut arguments: Vec<OsString> = vec!["--force".into(), "--sign".into(), "-".into()];
    if runtime {
        arguments.push("--options".into());
        arguments.push("runtime".into());
    }
    if let Some(entitlements) = entitlements {
        arguments.push("--entitlements".into());
        arguments.push(entitlements.as_os_str().to_os_string());
    }
    arguments.push(path.as_os_str().to_os_string());
    let borrowed: Vec<&OsStr> = arguments.iter().map(OsString::as_os_str).collect();
    tool("codesign", &borrowed).map(|_| ())
}

#[cfg(not(target_os = "macos"))]
pub fn codesign_sign(
    _path: &Path,
    _entitlements: Option<&Path>,
    _runtime: bool,
) -> Result<(), String> {
    Err("codesign signing requires macOS".into())
}

/// `ditto` preserves resource forks, ACLs and extended attributes.
#[cfg(target_os = "macos")]
pub fn ditto(source: &Path, destination: &Path) -> Result<(), String> {
    tool("ditto", &[source.as_os_str(), destination.as_os_str()]).map(|_| ())
}

#[cfg(not(target_os = "macos"))]
pub fn ditto(_source: &Path, _destination: &Path) -> Result<(), String> {
    Err("ditto requires macOS".into())
}

/// `CFBundleIdentifier` from the selected bundle.
#[cfg(target_os = "macos")]
pub fn bundle_identifier(app: &Path) -> Result<String, String> {
    let plist = app.join("Contents/Info.plist");
    let value = tool(
        "plutil",
        &[
            OsStr::new("-extract"),
            OsStr::new("CFBundleIdentifier"),
            OsStr::new("raw"),
            OsStr::new("-o"),
            OsStr::new("-"),
            plist.as_os_str(),
        ],
    )?;
    let identifier = value.trim();
    if identifier.is_empty() {
        return Err(format!(
            "{} has an empty CFBundleIdentifier",
            plist.display()
        ));
    }
    Ok(identifier.to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn bundle_identifier(_app: &Path) -> Result<String, String> {
    Err("reading Info.plist requires macOS".into())
}

/// Processes whose executable is the selected stub.
///
/// The comparison is by canonical executable path, never by process name.
#[cfg(target_os = "macos")]
pub fn running_executables(stub: &Path) -> Result<Vec<u32>, String> {
    use std::os::unix::ffi::OsStrExt;

    // `sys/proc_info.h`: `#define PROC_ALL_PIDS 1`.
    const PROC_ALL_PIDS: u32 = 1;

    // During a transaction the stub may be renamed away; the recorded path is
    // still the identity a previously started process reports.
    let wanted = fs::canonicalize(stub).unwrap_or_else(|_| stub.to_path_buf());
    let mut buffer: Vec<libc::pid_t> = Vec::new();
    for _ in 0..3 {
        let bytes = unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
        if bytes < 0 {
            return Err("proc_listpids failed".into());
        }
        let capacity = bytes as usize / std::mem::size_of::<libc::pid_t>() + 64;
        buffer.resize(capacity, 0);
        let written = unsafe {
            libc::proc_listpids(
                PROC_ALL_PIDS,
                0,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
            )
        };
        if written < 0 {
            return Err("proc_listpids failed".into());
        }
        buffer.truncate(written as usize / std::mem::size_of::<libc::pid_t>());
        if buffer.len() + 8 < capacity {
            break;
        }
    }

    let mut found = Vec::new();
    for pid in buffer {
        if pid <= 0 {
            continue;
        }
        let mut path = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let length =
            unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
        if length <= 0 {
            continue;
        }
        path.truncate(length as usize);
        let Some(text) = path.split(|byte| *byte == 0).next() else {
            continue;
        };
        let process_path = PathBuf::from(OsStr::from_bytes(text));
        let process_path = fs::canonicalize(&process_path).unwrap_or(process_path);
        if process_path == wanted {
            found.push(pid as u32);
        }
    }
    found.sort_unstable();
    Ok(found)
}

#[cfg(not(target_os = "macos"))]
pub fn running_executables(_stub: &Path) -> Result<Vec<u32>, String> {
    Err("the process identity check requires macOS".into())
}
