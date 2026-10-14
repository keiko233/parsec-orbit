//! Shared-persistent redirection for the activated vendor host.
//!
//! The client reads and writes `/Users/Shared/.parsec-persistent` independently
//! of the current-UID home. The isolation profile denies that real path, so
//! the path-taking APIs measured in the client import inventory are
//! interposed and rewritten to `<Orbit root>/home/.parsec-persistent`.
//!
//! The rewrite is deliberately conservative and never the security boundary:
//! only paths that start with the configured prefix at a component boundary
//! and contain no `..` component are rewritten. Anything else falls through
//! to the real path, where the sandbox profile denies it (fail closed).

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::OnceLock;

/// The vendor shared-persistent root this module redirects by default.
pub const SHARED_PERSISTENT: &str = "/Users/Shared/.parsec-persistent";

#[derive(Debug)]
struct Redirect {
    prefix: Vec<u8>,
    target: Vec<u8>,
}

static ACTIVE: OnceLock<Redirect> = OnceLock::new();

/// Installs the rewrite. Only the explicit test harness and the bootstrap
/// call this; an unrelated host never reaches it.
pub fn install(prefix: &Path, target: &Path) -> Result<(), String> {
    if ACTIVE.get().is_some() {
        return Err("shared-persistent redirect already installed".into());
    }
    if !prefix.is_absolute() {
        return Err(format!(
            "redirect prefix {} is not absolute",
            prefix.display()
        ));
    }
    if !target.is_absolute() {
        return Err(format!(
            "redirect target {} is not absolute",
            target.display()
        ));
    }
    let mut prefix = prefix.as_os_str().as_bytes().to_vec();
    while prefix.len() > 1 && prefix.last() == Some(&b'/') {
        prefix.pop();
    }
    let target = target.as_os_str().as_bytes().to_vec();
    ACTIVE
        .set(Redirect { prefix, target })
        .map_err(|_| "shared-persistent redirect already installed".to_string())
}

fn rewrite_with(redirect: &Redirect, path: &CStr) -> Option<CString> {
    let bytes = path.to_bytes();
    let rest = bytes.strip_prefix(redirect.prefix.as_slice())?;
    if !rest.is_empty() && !rest.starts_with(b"/") {
        // "/Users/Shared/.parsec-persistent-evil" is a different path.
        return None;
    }
    if rest
        .split(|byte| *byte == b'/')
        .any(|component| component == b"..")
    {
        return None;
    }
    let mut out = redirect.target.clone();
    out.extend_from_slice(rest);
    CString::new(out).ok()
}

/// Returns the rewritten path when the redirect is active and safely applies.
fn redirect(path: *const c_char) -> Option<CString> {
    let active = ACTIVE.get()?;
    if path.is_null() {
        return None;
    }
    // Safety: libc callers pass a NUL-terminated path.
    let path = unsafe { CStr::from_ptr(path) };
    rewrite_with(active, path)
}

// Each replacement is spelled out so rustc checks every signature against
// libc. dyld interposition excludes this image, so the libc calls inside the
// replacements resolve the original functions.
//
// `open` and `openat` are variadic. On Darwin arm64 a variadic call site
// passes the mode argument on the stack, so the fixed-signature
// implementations below receive it through the trampolines in
// `orbit_redirect_trampolines`; on other arches the fixed signature already
// matches the call site.
#[no_mangle]
unsafe extern "C" fn orbit_redirect_open_impl(
    path: *const c_char,
    oflag: c_int,
    mode: libc::mode_t,
) -> c_int {
    match redirect(path) {
        // Without O_CREAT the mode register is ignored by the original call;
        // `open` is variadic in libc, so mode promotes like the C default.
        Some(rewritten) => libc::open(rewritten.as_ptr(), oflag, mode as libc::c_uint),
        None => libc::open(path, oflag, mode as libc::c_uint),
    }
}

#[no_mangle]
unsafe extern "C" fn orbit_redirect_openat_impl(
    dirfd: c_int,
    path: *const c_char,
    oflag: c_int,
    mode: libc::mode_t,
) -> c_int {
    // Only absolute paths are rewritten; a relative path keeps its meaning.
    // `openat` is variadic in libc; mode promotes like the C default.
    match redirect(path) {
        Some(rewritten) => libc::openat(dirfd, rewritten.as_ptr(), oflag, mode as libc::c_uint),
        None => libc::openat(dirfd, path, oflag, mode as libc::c_uint),
    }
}

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".globl _orbit_redirect_open",
    "_orbit_redirect_open:",
    // First variadic argument per the Darwin arm64 calling convention.
    "ldr x2, [sp]",
    "b _orbit_redirect_open_impl",
    ".globl _orbit_redirect_openat",
    "_orbit_redirect_openat:",
    "ldr x3, [sp]",
    "b _orbit_redirect_openat_impl",
);

#[cfg(target_arch = "aarch64")]
unsafe extern "C" {
    fn orbit_redirect_open();
    fn orbit_redirect_openat();
}

#[cfg(target_arch = "aarch64")]
const OPEN_REPLACEMENT: *const c_void = orbit_redirect_open as *const () as _;
#[cfg(not(target_arch = "aarch64"))]
const OPEN_REPLACEMENT: *const c_void = orbit_redirect_open_impl as *const () as _;
#[cfg(target_arch = "aarch64")]
const OPENAT_REPLACEMENT: *const c_void = orbit_redirect_openat as *const () as _;
#[cfg(not(target_arch = "aarch64"))]
const OPENAT_REPLACEMENT: *const c_void = orbit_redirect_openat_impl as *const () as _;

unsafe extern "C" fn fopen(path: *const c_char, mode: *const c_char) -> *mut libc::FILE {
    match redirect(path) {
        Some(rewritten) => libc::fopen(rewritten.as_ptr(), mode),
        None => libc::fopen(path, mode),
    }
}

unsafe extern "C" fn stat(path: *const c_char, buffer: *mut libc::stat) -> c_int {
    match redirect(path) {
        Some(rewritten) => libc::stat(rewritten.as_ptr(), buffer),
        None => libc::stat(path, buffer),
    }
}

unsafe extern "C" fn lstat(path: *const c_char, buffer: *mut libc::stat) -> c_int {
    match redirect(path) {
        Some(rewritten) => libc::lstat(rewritten.as_ptr(), buffer),
        None => libc::lstat(path, buffer),
    }
}

unsafe extern "C" fn access(path: *const c_char, amode: c_int) -> c_int {
    match redirect(path) {
        Some(rewritten) => libc::access(rewritten.as_ptr(), amode),
        None => libc::access(path, amode),
    }
}

unsafe extern "C" fn mkdir(path: *const c_char, mode: libc::mode_t) -> c_int {
    match redirect(path) {
        Some(rewritten) => libc::mkdir(rewritten.as_ptr(), mode),
        None => libc::mkdir(path, mode),
    }
}

unsafe extern "C" fn rename(old: *const c_char, new: *const c_char) -> c_int {
    let old_rewritten = redirect(old);
    let new_rewritten = redirect(new);
    let old_ptr = old_rewritten.as_ref().map_or(old, |value| value.as_ptr());
    let new_ptr = new_rewritten.as_ref().map_or(new, |value| value.as_ptr());
    libc::rename(old_ptr, new_ptr)
}

unsafe extern "C" fn opendir(path: *const c_char) -> *mut libc::DIR {
    match redirect(path) {
        Some(rewritten) => libc::opendir(rewritten.as_ptr()),
        None => libc::opendir(path),
    }
}

#[repr(C)]
struct Interpose {
    replacement: *const c_void,
    original: *const c_void,
}
// Immutable relocations written by dyld, not mutable application pointers.
unsafe impl Sync for Interpose {}

#[used]
#[link_section = "__DATA,__interpose"]
static INTERPOSE: [Interpose; 9] = [
    Interpose {
        replacement: OPEN_REPLACEMENT,
        original: libc::open as *const () as _,
    },
    Interpose {
        replacement: OPENAT_REPLACEMENT,
        original: libc::openat as *const () as _,
    },
    Interpose {
        replacement: fopen as *const () as _,
        original: libc::fopen as *const () as _,
    },
    Interpose {
        replacement: stat as *const () as _,
        original: libc::stat as *const () as _,
    },
    Interpose {
        replacement: lstat as *const () as _,
        original: libc::lstat as *const () as _,
    },
    Interpose {
        replacement: access as *const () as _,
        original: libc::access as *const () as _,
    },
    Interpose {
        replacement: mkdir as *const () as _,
        original: libc::mkdir as *const () as _,
    },
    Interpose {
        replacement: rename as *const () as _,
        original: libc::rename as *const () as _,
    },
    Interpose {
        replacement: opendir as *const () as _,
        original: libc::opendir as *const () as _,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn redirect_for(prefix: &str, target: &str) -> Redirect {
        Redirect {
            prefix: prefix.as_bytes().to_vec(),
            target: target.as_bytes().to_vec(),
        }
    }

    fn rewrite(prefix: &str, target: &str, path: &str) -> Option<String> {
        let redirect = redirect_for(prefix, target);
        let path = CString::new(path).unwrap();
        rewrite_with(&redirect, &path).map(|out| out.to_string_lossy().into_owned())
    }

    #[test]
    fn the_prefix_and_its_children_are_rewritten() {
        let prefix = "/Users/Shared/.parsec-persistent";
        let target = "/orbit/home/.parsec-persistent";
        assert_eq!(rewrite(prefix, target, prefix).as_deref(), Some(target));
        assert_eq!(
            rewrite(prefix, target, "/Users/Shared/.parsec-persistent/devid.bin").as_deref(),
            Some("/orbit/home/.parsec-persistent/devid.bin")
        );
    }

    #[test]
    fn other_paths_and_escape_spellings_are_not_rewritten() {
        let prefix = "/Users/Shared/.parsec-persistent";
        let target = "/orbit/home/.parsec-persistent";
        assert_eq!(rewrite(prefix, target, "/Users/Shared/.parsec"), None);
        assert_eq!(
            rewrite(prefix, target, "/Users/Shared/.parsec-persistent-evil/x"),
            None
        );
        assert_eq!(
            rewrite(prefix, target, "/Users/Shared/.parsec-persistent/../other"),
            None
        );
        assert_eq!(rewrite(prefix, target, "/tmp/elsewhere"), None);
    }

    #[test]
    fn install_validates_and_latches_once() {
        // The gate is process-global, so this test asserts the validation
        // errors without mutating the latch.
        assert!(install(Path::new("relative"), Path::new("/orbit/target")).is_err());
        assert!(install(Path::new("/prefix"), Path::new("relative")).is_err());
    }
}
