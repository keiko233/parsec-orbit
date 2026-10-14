//! Minimal dependency constructor for an activated vendor host.
//!
//! Order matters and every step fails closed: the shared-persistent redirect,
//! the custom-SBPL isolation profile with its sentinel gate and the
//! current-UID home replacement must all be installed and verified before the
//! constructor returns; anything else exits the process before vendor main.
//! The readiness wait and the plugin scan run on the deferred worker, never
//! on this thread (`docs/spec/macos-host.md`, sections 6.1-6.3).

use std::ffi::{c_void, CStr};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

fn loader_path() -> Option<PathBuf> {
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    let found = unsafe { libc::dladdr(initialize as *const () as *const c_void, &mut info) };
    if found == 0 || info.dli_fname.is_null() {
        return None;
    }
    let bytes = unsafe { CStr::from_ptr(info.dli_fname) }.to_bytes();
    std::fs::canonicalize(Path::new(std::ffi::OsStr::from_bytes(bytes))).ok()
}

fn selected_host(executable: &Path, loader: &Path) -> bool {
    let Some(contents) = loader.parent().and_then(Path::parent) else {
        return false;
    };
    loader == contents.join("Frameworks/libparsec_orbit_loader.dylib")
        && executable == contents.join("MacOS/parsecd")
}

fn check() -> Result<(), String> {
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("cannot identify executable: {error}"))?;
    let Some(loader) = loader_path() else {
        // A possible vendor host cannot proceed without identifying the
        // dependency that must protect it. Unrelated hosts stay inactive.
        if executable.file_name().is_some_and(|name| name == "parsecd") {
            return Err("cannot identify Orbit loader in a possible client host".into());
        }
        return Ok(());
    };
    // Unrelated hosts do not read an installation record, query NSS, create
    // directories, initialize logging or start an Orbit thread.
    if !selected_host(&executable, &loader) {
        return Ok(());
    }
    let home = crate::config::real_home().ok_or("cannot resolve real current-UID home")?;
    let activation =
        crate::activation::inspect(&executable, &loader, unsafe { libc::getuid() }, &home)?;
    let Some(activation) = activation else {
        return Err("selected host lost its installation binding".into());
    };
    // Install the measured isolation primitives before anything vendor-owned
    // can run. Any failure exits 78 in `initialize`, before vendor main.
    let persistent = activation.isolated_home.join(".parsec-persistent");
    crate::redirect::install(Path::new(crate::redirect::SHARED_PERSISTENT), &persistent)
        .map_err(|error| format!("cannot install shared-persistent redirect: {error}"))?;
    let sentinel = crate::isolation::sentinel_path(&activation.orbit_root);
    let profile =
        crate::isolation::build_profile(&crate::isolation::protected_roots(&home), &sentinel)?;
    crate::isolation::install(&profile, &sentinel)
        .map_err(|error| format!("cannot install the isolation guard: {error}"))?;
    crate::runtime::arm_isolated_home(&activation)
        .map_err(|error| format!("cannot arm the isolated home: {error}"))?;
    crate::runtime::start_deferred(&activation)
        .map_err(|error| format!("cannot start the Orbit runtime: {error}"))?;
    Ok(())
}

unsafe extern "C" fn initialize() {
    if let Err(reason) = check() {
        let message = format!("Parsec Orbit: {reason}\n");
        libc::write(libc::STDERR_FILENO, message.as_ptr().cast(), message.len());
        libc::_exit(78);
    }
}

#[used]
#[link_section = "__DATA,__mod_init_func"]
static CONSTRUCTOR: unsafe extern "C" fn() = initialize;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_the_bound_sibling_layout_selects_a_host() {
        let loader =
            Path::new("/private/scratch/Test.app/Contents/Frameworks/libparsec_orbit_loader.dylib");
        assert!(selected_host(
            Path::new("/private/scratch/Test.app/Contents/MacOS/parsecd"),
            loader
        ));
        assert!(!selected_host(
            Path::new("/Applications/Parsec.app/Contents/MacOS/parsecd"),
            loader
        ));
        assert!(!selected_host(
            Path::new("/private/scratch/Test.app/Contents/MacOS/other"),
            loader
        ));
        assert!(!selected_host(
            Path::new("/private/scratch/Test.app/Contents/MacOS/parsecd"),
            Path::new("/private/scratch/loader.dylib")
        ));
    }
}
