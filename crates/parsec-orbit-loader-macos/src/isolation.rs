//! Fail-closed custom-SBPL isolation for an activated vendor host.
//!
//! The measured mechanism (see
//! `docs/design/macos/isolation-mechanism-assessment.md`) installs a custom
//! sandbox profile from the loader constructor, then proves the policy is
//! active by probing an owned sentinel file the profile denies. Every
//! failure path exits the caller before vendor code can run: an installation
//! error, an unexpected sentinel error, or a sentinel that stays readable.
//!
//! The profile keeps `(allow default)` and adds literal denies for the real
//! Parsec state roots plus a sentinel read deny. The unsupported
//! `sandbox_init` API and its SDK status are recorded in the assessment; the
//! sentinel gate is what keeps an ineffective API fail-closed.
//!
//! Mach lookups intentionally stay on `(allow default)`: the measured
//! allowlist form works for an owned service, but a blanket `mach-lookup`
//! deny also blocked a standard system service, and the vendor client's
//! required service set is not enumerable without running it. A
//! `network-outbound` deny for unix sockets under each protected root is
//! included because the path-only rules left a connect reachable (measured).

use std::ffi::{c_char, CStr, CString};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Name prefix of the per-launch sentinel file inside the Orbit root.
pub const SENTINEL_PREFIX: &str = ".isolation-sentinel";

/// A compiled SBPL profile text, ready for `sandbox_init`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    text: String,
}

impl Profile {
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Where the sentinel goes for this launch.
pub fn sentinel_path(orbit_root: &Path) -> PathBuf {
    orbit_root.join(format!("{SENTINEL_PREFIX}.{}", std::process::id()))
}

/// The real account state the vendor client must never read or write.
pub fn protected_roots(real_home: &Path) -> Vec<PathBuf> {
    vec![
        real_home.join(".parsec"),
        real_home.join(".parsec-persistent"),
        PathBuf::from("/Users/Shared/.parsec"),
        PathBuf::from("/Users/Shared/.parsec-persistent"),
    ]
}

fn sbpl_quote(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 2);
    for byte in bytes {
        match byte {
            b'\\' | b'"' => {
                out.push('\\');
                out.push(*byte as char);
            }
            _ => out.push(*byte as char),
        }
    }
    out
}

fn checked_path(path: &Path, what: &str) -> Result<Vec<u8>, String> {
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute() {
        return Err(format!("{what} {} is not absolute", path.display()));
    }
    if path == Path::new("/") {
        return Err(format!("{what} may not be the filesystem root"));
    }
    if bytes.contains(&0) {
        return Err(format!("{what} contains a NUL byte"));
    }
    Ok(bytes.to_vec())
}

/// Builds the measured deny profile for the given protected roots and the
/// per-launch sentinel.
pub fn build_profile(roots: &[PathBuf], sentinel: &Path) -> Result<Profile, String> {
    let mut unique: Vec<Vec<u8>> = Vec::new();
    for root in roots {
        let bytes = checked_path(root, "protected root")?;
        if !unique.contains(&bytes) {
            unique.push(bytes);
        }
    }
    if unique.is_empty() {
        return Err("no protected roots were provided".into());
    }
    let sentinel = checked_path(sentinel, "sentinel path")?;
    let mut text = String::from("(version 1)\n(allow default)\n");
    for root in &unique {
        let quoted = sbpl_quote(root);
        text.push_str(&format!("(deny file-read* (subpath \"{quoted}\"))\n"));
        text.push_str(&format!("(deny file-write* (subpath \"{quoted}\"))\n"));
    }
    // Unix-domain sockets are not covered by file-read/file-write rules
    // (measured: a connect to a socket under a denied root succeeded).
    for root in &unique {
        let quoted = sbpl_quote(root);
        text.push_str(&format!(
            "(deny network-outbound (remote unix-socket (subpath \"{quoted}\")))\n"
        ));
    }
    text.push_str(&format!(
        "(deny file-read* (literal \"{}\"))\n",
        sbpl_quote(&sentinel)
    ));
    Ok(Profile { text })
}

// The raw sandbox_init binding. Flags stay 0: the measured mechanism uses a
// custom profile string, not a named profile.
unsafe extern "C" {
    fn sandbox_init(profile: *const c_char, flags: u64, error: *mut *mut c_char) -> i32;
    fn sandbox_free_error(error: *mut c_char);
}

fn call_sandbox_init(profile: &CStr) -> Result<(), String> {
    let mut error: *mut c_char = std::ptr::null_mut();
    // Safety: `profile` is a valid NUL-terminated string. `error` receives an
    // owned message when the call fails.
    let result = unsafe { sandbox_init(profile.as_ptr(), 0, &mut error) };
    if result == 0 {
        return Ok(());
    }
    let errno = std::io::Error::last_os_error();
    let detail = if error.is_null() {
        "(no detail)".to_string()
    } else {
        // Safety: the API returns an owned C string when `error` is set.
        let text = unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned();
        unsafe { sandbox_free_error(error) };
        text
    };
    Err(format!("sandbox_init rc={result} errno={errno}: {detail}"))
}

/// Installs the profile and requires the sentinel to be denied afterwards.
///
/// Kept separate from [`install`] so tests can inject an installer without
/// applying a real sandbox to the test process.
fn install_with(
    profile: &Profile,
    sentinel: &Path,
    installer: impl Fn(&CStr) -> Result<(), String>,
) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(sentinel)
        .map_err(|error| format!("cannot create sentinel {}: {error}", sentinel.display()))?;
    let marker = format!("orbit-isolation-{}\n", std::process::id());
    let created = file
        .write_all(marker.as_bytes())
        .map_err(|error| format!("cannot write sentinel {}: {error}", sentinel.display()));
    drop(file);
    let cleanup = || {
        let _ = fs::remove_file(sentinel);
    };
    if let Err(error) = created {
        cleanup();
        return Err(error);
    }
    let text = CString::new(profile.text().as_bytes().to_vec())
        .map_err(|_| "profile contains a NUL byte".to_string());
    let text = match text {
        Ok(text) => text,
        Err(error) => {
            cleanup();
            return Err(error);
        }
    };
    if let Err(error) = installer(&text) {
        cleanup();
        return Err(error);
    }
    match fs::File::open(sentinel) {
        Ok(_) => {
            cleanup();
            Err("installed profile still allows the sentinel read".into())
        }
        Err(error) if matches!(error.raw_os_error(), Some(libc::EPERM) | Some(libc::EACCES)) => {
            cleanup();
            Ok(())
        }
        Err(error) => {
            cleanup();
            Err(format!("sentinel probe failed with {error}"))
        }
    }
}

/// Installs the profile into this process and verifies the sentinel gate.
pub fn install(profile: &Profile, sentinel: &Path) -> Result<(), String> {
    install_with(profile, sentinel, call_sandbox_init)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_core::testutil::TempDir;

    fn roots() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/Users/example/.parsec"),
            PathBuf::from("/Users/example/.parsec-persistent"),
            PathBuf::from("/Users/Shared/.parsec"),
            PathBuf::from("/Users/Shared/.parsec-persistent"),
        ]
    }

    #[test]
    fn the_profile_orders_allow_default_before_every_deny() {
        let profile = build_profile(&roots(), Path::new("/orbit/.isolation-sentinel.1")).unwrap();
        let text = profile.text();
        assert!(text.starts_with("(version 1)\n(allow default)\n"));
        for root in roots() {
            let root = root.display();
            assert!(text.contains(&format!("(deny file-read* (subpath \"{root}\"))")));
            assert!(text.contains(&format!("(deny file-write* (subpath \"{root}\"))")));
        }
        assert!(text.contains("(deny network-outbound (remote unix-socket"));
        assert!(text.ends_with("(deny file-read* (literal \"/orbit/.isolation-sentinel.1\"))\n"));
    }

    #[test]
    fn duplicate_roots_collapse_and_invalid_paths_are_rejected() {
        let mut roots = roots();
        roots.push(PathBuf::from("/Users/Shared/.parsec"));
        let profile = build_profile(&roots, Path::new("/orbit/sentinel")).unwrap();
        // One read rule, one write rule and one socket rule for the deduplicated root.
        assert_eq!(profile.text().matches("/Users/Shared/.parsec\"").count(), 3);
        assert!(build_profile(&[PathBuf::from("relative/.parsec")], Path::new("/s")).is_err());
        assert!(build_profile(&[PathBuf::from("/")], Path::new("/s")).is_err());
        assert!(build_profile(&[], Path::new("/s")).is_err());
        assert!(build_profile(&roots, Path::new("relative-sentinel")).is_err());
    }

    #[test]
    fn quotes_and_backslashes_are_escaped() {
        let profile = build_profile(
            &[PathBuf::from("/Users/od\"d/back\\slash")],
            Path::new("/orbit/sentinel"),
        )
        .unwrap();
        assert!(profile.text().contains("od\\\"d/back\\\\slash"));
    }

    #[test]
    fn an_installer_error_removes_the_sentinel_and_fails() {
        let scratch = TempDir::new("isolation-gate");
        let sentinel = scratch.path().join("sentinel");
        let profile = build_profile(&roots(), &sentinel).unwrap();
        let result = install_with(&profile, &sentinel, |_| Err("simulated failure".into()));
        assert!(result.unwrap_err().contains("simulated failure"));
        assert!(!sentinel.exists());
    }

    #[test]
    fn a_silently_ineffective_installer_fails_the_gate() {
        let scratch = TempDir::new("isolation-gate");
        let sentinel = scratch.path().join("sentinel");
        let profile = build_profile(&roots(), &sentinel).unwrap();
        let result = install_with(&profile, &sentinel, |_| Ok(()));
        assert!(result
            .unwrap_err()
            .contains("still allows the sentinel read"));
        assert!(!sentinel.exists());
    }

    #[test]
    fn the_sentinel_name_is_private_and_unique_per_process() {
        let path = sentinel_path(Path::new("/orbit"));
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(SENTINEL_PREFIX));
        assert!(name.ends_with(&std::process::id().to_string()));
    }
}
