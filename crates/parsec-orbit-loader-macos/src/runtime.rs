//! Deferred client-runtime wiring for an activated, guarded host.
//!
//! The dependency constructor must return quickly: the vendor stub and its
//! client run on the main thread right after it. Home replacement has to be
//! armed before any vendor code runs, but the readiness wait and the plugin
//! initialization belong on one dedicated worker
//! (`docs/spec/macos-host.md`, section 6.3; `docs/design/macos-phases.md`,
//! M1a).
//!
//! Every step here fails closed: a missing or inactive home replacement, a
//! worker that cannot start, or readiness evidence that never appears means
//! the caller refuses before vendor main, and a worker that starts but never
//! sees the client only skips plugin initialization. Nothing in this module
//! weakens the isolation profile or the redirect.

use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::activation::Activation;
use crate::config::Config;
use crate::log_observer::LogObserver;
use crate::readiness::{self, ClientBuild};

/// The vendor log inside the isolated state root. The measured release
/// announcement `Parsec release (<version>-<build>, ..., Loader: 14)` is
/// appended here (`docs/design/macos/smoke-test.md`).
pub const VENDOR_LOG: &str = "log.txt";

/// `<orbit_root>/home/.parsec`: the private state root the vendor stub seeds
/// and the client loads its dylib from once the home replacement is active.
pub fn isolated_state(activation: &Activation) -> PathBuf {
    activation.isolated_home.join(".parsec")
}

/// Arms the current-UID home replacement and proves it is active.
///
/// Must run after the isolation profile is installed and verified: the
/// profile is the boundary, this is the mechanism that makes the vendor stub
/// resolve the private home inside it.
pub fn arm_isolated_home(activation: &Activation) -> Result<(), String> {
    let uid = unsafe { libc::getuid() };
    let home = CString::new(activation.isolated_home.as_os_str().as_bytes())
        .map_err(|_| "isolated home contains a NUL byte".to_string())?;
    crate::nss::install(uid, home)?;
    verify_home_replacement(uid, &activation.isolated_home)
}

/// Proves the interposed lookup returns the isolated home and that the real
/// NSS entry still resolves to a different, real home.
///
/// In-process only: cross-image interposition itself is measured by the
/// separate-image probe in `tests/bootstrap_e2e.rs`.
fn verify_home_replacement(uid: libc::uid_t, expected: &Path) -> Result<(), String> {
    // Direct call into the interposed implementation (this image's own
    // imports are not rewritten by dyld, so `libc::getpwuid` below stays the
    // real NSS entry).
    let replaced = unsafe { crate::nss::getpwuid(uid) };
    if replaced.is_null() {
        return Err("home replacement lookup returned null".into());
    }
    let replaced_dir = unsafe { CStr::from_ptr((*replaced).pw_dir) };
    let replaced_dir = Path::new(std::ffi::OsStr::from_bytes(replaced_dir.to_bytes()));
    if replaced_dir != expected {
        return Err(format!(
            "home replacement is inactive: got {}, expected {}",
            replaced_dir.display(),
            expected.display()
        ));
    }
    let real = unsafe { libc::getpwuid(uid) };
    if real.is_null() {
        return Err("real NSS lookup returned null after replacement".into());
    }
    let real_dir = unsafe { CStr::from_ptr((*real).pw_dir) };
    let real_dir = Path::new(std::ffi::OsStr::from_bytes(real_dir.to_bytes()));
    if real_dir == expected {
        return Err("the real NSS entry already names the isolated home".into());
    }
    Ok(())
}

/// Starts the single deferred worker and returns once the thread exists.
///
/// The caller keeps the process alive; the worker waits for the readiness
/// policy and then initializes Orbit off the client's hot path.
pub fn start_deferred(activation: &Activation) -> Result<(), String> {
    let uid = unsafe { libc::getuid() };
    let state_root = isolated_state(activation);
    let config = Config::for_root(activation.orbit_root.clone());
    std::thread::Builder::new()
        .name("parsec-orbit-runtime".into())
        .spawn(move || run_deferred(&state_root, &config, uid))
        .map(|_| ())
        .map_err(|error| format!("cannot start the Orbit runtime worker: {error}"))
}

fn run_deferred(state_root: &Path, config: &Config, uid: u32) {
    let init = crate::log::init(&config.log_candidates);
    if init.path.is_none() {
        // Without a log destination there is nowhere to report anything and
        // no reason to hash or scan; the client itself is unaffected.
        return;
    }
    match await_ready_with(state_root, uid, crate::original::mapped_images) {
        Ok((build, image)) => {
            match crate::client::install(&build, &image) {
                Ok(()) => crate::log::info(&format!(
                    "client {}-{} detected; {}/{} registered",
                    build.version,
                    build.build,
                    crate::client::INTERFACE_CLIENT,
                    image.display()
                )),
                Err(error) => {
                    crate::log::warn(&format!("client information unavailable: {error}"));
                    return;
                }
            }
            let _ = crate::scan(config);
        }
        Err(reason) => crate::log::warn(&format!("client readiness not observed: {reason}")),
    }
}

/// Waits for the readiness policy with injectable image observation.
///
/// The production worker passes the live dyld snapshot; tests pass owned
/// paths, so a synthetic state root exercises the whole probe without mapping
/// a client image.
fn await_ready_with(
    state_root: &Path,
    uid: u32,
    images: impl FnMut() -> Vec<PathBuf>,
) -> Result<(ClientBuild, PathBuf), String> {
    await_ready_until(
        state_root,
        uid,
        Instant::now() + readiness::READINESS_TIMEOUT,
        images,
    )
}

fn await_ready_until(
    state_root: &Path,
    uid: u32,
    deadline: Instant,
    mut images: impl FnMut() -> Vec<PathBuf>,
) -> Result<(ClientBuild, PathBuf), String> {
    // The vendor stub creates the state root and seeds the client itself, so
    // the root may not exist yet when the worker starts. Keep waiting for it,
    // but fail permanently once it exists in an unsafe shape.
    let mut observer: Option<LogObserver> = None;
    let mut snapshot: Vec<PathBuf> = Vec::new();
    let build = readiness::wait_until_ready(
        || {
            if observer.is_none() {
                match std::fs::symlink_metadata(state_root) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return crate::readiness::ProbeState::NotYet(
                            "the client has not created its private state root yet".into(),
                        );
                    }
                    _ => {}
                }
                match LogObserver::snapshot(state_root, VENDOR_LOG, uid) {
                    Ok(found) => observer = Some(found),
                    Err(reason) => return crate::readiness::ProbeState::Failed(reason),
                }
            }
            snapshot = images();
            observer
                .as_mut()
                .expect("the observer was just installed")
                .observe_client_state(&snapshot)
        },
        deadline,
        std::thread::sleep,
    )?;
    let image = snapshot
        .iter()
        .find(|path| {
            path.parent() == Some(state_root)
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(readiness::parse_client_name)
                    .as_ref()
                    == Some(&build)
        })
        .cloned()
        .ok_or("the ready client image vanished from the dyld snapshot")?;
    Ok((build, image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    const ANNOUNCEMENT: &str =
        "[D 2026-10-13 08:03:58] log: Parsec release (150-101a, Service: -1, Loader: 14)\n";

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "parsec-orbit-runtime-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }

        fn state(&self) -> PathBuf {
            let state = self.0.join(".parsec");
            std::fs::create_dir(&state).unwrap();
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
            state
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_matching_image_and_announcement_select_the_build() {
        let scratch = Scratch::new();
        let state = scratch.state();
        let log = state.join(VENDOR_LOG);
        // The observer snapshots the pre-launch length: create the log first,
        // then let the first probe append the announcement like the vendor
        // client does after it starts.
        std::fs::write(&log, b"").unwrap();
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o600)).unwrap();
        let image = state.join("parsecd-150-101a.dylib");
        let mut announced = false;
        let (build, selected) = await_ready_with(&state, unsafe { libc::getuid() }, || {
            if !announced {
                std::fs::write(&log, ANNOUNCEMENT).unwrap();
                announced = true;
            }
            vec![PathBuf::from("/usr/lib/libSystem.B.dylib"), image.clone()]
        })
        .unwrap();
        assert_eq!(build.version, "150");
        assert_eq!(build.build, "101a");
        assert_eq!(selected, image);
    }

    #[test]
    fn an_image_without_the_announcement_keeps_waiting_then_times_out() {
        let scratch = Scratch::new();
        let state = scratch.state();
        let image = state.join("parsecd-150-101a.dylib");
        let result = await_ready_until(
            &state,
            unsafe { libc::getuid() },
            Instant::now() + std::time::Duration::from_millis(50),
            || vec![image.clone()],
        );
        assert!(result.unwrap_err().contains("timed out"));
    }

    #[test]
    fn a_state_root_created_after_start_is_picked_up() {
        let scratch = Scratch::new();
        let state = scratch.0.join(".parsec");
        let log = state.join(VENDOR_LOG);
        let image = state.join("parsecd-150-101a.dylib");
        // The vendor stub creates the private root and its empty log, then
        // the client announces its release later. A helper thread plays that
        // sequence while the worker polls.
        let feeder = {
            let state = state.clone();
            let log = log.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&state)
                    .unwrap();
                std::fs::write(&log, b"").unwrap();
                std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o600)).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(280));
                std::fs::write(&log, ANNOUNCEMENT).unwrap();
            })
        };
        let (build, selected) =
            await_ready_with(&state, unsafe { libc::getuid() }, || vec![image.clone()]).unwrap();
        feeder.join().unwrap();
        assert_eq!(build.version, "150");
        assert_eq!(selected, image);
    }
}
