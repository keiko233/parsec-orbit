//! The `parsec-orbit.client` service (`docs/spec/macos-platform.md`, section 6).
//!
//! Registered by the loader before any plugin runs, so a plugin can learn
//! which client build it is inside and refuse unknown internals. The build is
//! only reported after the readiness policy detected a mapped client image
//! under the isolated state and a matching current-launch release
//! announcement; the file hash is read from that mapped image.
//!
//! `status` is `"not-verified"` for every detected build at M1a: no verified
//! build whitelist exists yet (that is M2 work together with the
//! internals-dependent plugins). `"incompatible"` is reserved for a build the
//! loader knows is unsupported; `info` returns `-4` when called before a
//! build was detected, which is when plugins must treat the status as
//! `"not-verified"` anyway.

use std::ffi::c_void;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use parsec_orbit_plugin_abi::{
    MessageCallback, ParsecOrbitInterfaceHeader, ERR_INVALID, ERR_NOT_READY, OK,
};
use sha2::{Digest, Sha256};

use crate::readiness::ClientBuild;

/// Reserved interface name (`docs/spec/services.md`).
pub const INTERFACE_CLIENT: &str = "parsec-orbit.client";
/// The `LOADER_V` token the measured client announces (`Loader: 14`).
pub const LOADER_TOKEN: u32 = 14;
/// Registry owner; reserved names need the official plugin-id prefix.
const OWNER: &str = "com.parsec-orbit.loader-macos";
/// Cap for hashing the mapped client image. The measured client dylib is
/// ~6 MiB; the cap keeps a swapped-in huge file from stalling the worker.
const MAX_HASH_BYTES: u64 = 64 * 1024 * 1024;

/// `parsec-orbit.client` version 1.0.
///
/// ```c
/// typedef struct ParsecOrbitClientV1 {
///     ParsecOrbitInterfaceHeader header;
///     int32_t (*info)(ParsecOrbitMessageCallback out, void *user);
/// } ParsecOrbitClientV1;
/// ```
#[repr(C)]
pub struct ParsecOrbitClientV1 {
    pub header: ParsecOrbitInterfaceHeader,
    /// Passes the build JSON to `out` on the caller's thread before
    /// returning. The callback is nullable at the ABI level and a null
    /// callback is reported as an invalid argument instead of crashing.
    pub info: unsafe extern "system" fn(out: Option<MessageCallback>, user: *mut c_void) -> i32,
}

static TABLE: ParsecOrbitClientV1 = ParsecOrbitClientV1 {
    header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitClientV1>(), 1, 0),
    info,
};

static INFO_JSON: OnceLock<String> = OnceLock::new();

unsafe extern "system" fn info(out: Option<MessageCallback>, user: *mut c_void) -> i32 {
    let Some(out) = out else {
        return ERR_INVALID;
    };
    let Some(json) = INFO_JSON.get() else {
        return ERR_NOT_READY;
    };
    out(user, json.as_ptr(), json.len());
    OK
}

/// Registers the service for `build`, hashing the mapped client image.
///
/// Called once by the deferred runtime worker after readiness, before the
/// plugin scan. A second call or an unreadable/oversized image fails rather
/// than registering incomplete information.
pub fn install(build: &ClientBuild, image: &Path) -> Result<(), String> {
    let digest = sha256_file(image)?;
    let json = serde_json::json!({
        "platform": "macos",
        "arch": "arm64",
        "build": format!("{}-{}", build.version, build.build),
        "dylib_sha256": digest,
        "loader_token": LOADER_TOKEN,
        "status": "not-verified",
    })
    .to_string();
    INFO_JSON
        .set(json)
        .map_err(|_| "client build information was already installed".to_string())?;
    let code = parsec_orbit_core::host::register(
        OWNER,
        INTERFACE_CLIENT,
        std::ptr::addr_of!(TABLE.header),
    );
    if code != OK {
        return Err(format!("cannot register {INTERFACE_CLIENT}: error {code}"));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if metadata.len() > MAX_HASH_BYTES {
        return Err(format!(
            "{}: {} bytes exceed the hash cap",
            path.display(),
            metadata.len()
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// The interface name this module owns, for tests that look it up.
#[cfg(test)]
fn registered_table() -> *const ParsecOrbitInterfaceHeader {
    parsec_orbit_core::host::find(INTERFACE_CLIENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_core::testutil::TempDir;

    unsafe extern "system" fn capture(user: *mut c_void, message: *const u8, len: usize) {
        let out = &mut *(user as *mut String);
        *out = String::from_utf8_lossy(std::slice::from_raw_parts(message, len)).into_owned();
    }

    #[test]
    fn the_service_reports_the_detected_build_and_hash() {
        let scratch = TempDir::new("client-service");
        let image = scratch.path().join("parsecd-150-101a.dylib");
        std::fs::write(&image, b"owned client fixture").unwrap();
        let build = ClientBuild {
            version: "150".into(),
            build: "101a".into(),
        };
        install(&build, &image).unwrap();

        let table = registered_table();
        assert!(!table.is_null(), "the service is registered");
        let table = table as *const ParsecOrbitClientV1;
        let mut json = String::new();
        let code = unsafe { ((*table).info)(Some(capture), (&mut json as *mut String).cast()) };
        assert_eq!(code, OK);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["platform"], "macos");
        assert_eq!(value["arch"], "arm64");
        assert_eq!(value["build"], "150-101a");
        assert_eq!(value["loader_token"], LOADER_TOKEN);
        assert_eq!(value["status"], "not-verified");
        assert_eq!(value["dylib_sha256"].as_str().unwrap().len(), 64);
        // A null callback is rejected instead of faulting.
        assert_eq!(
            unsafe { ((*table).info)(None, std::ptr::null_mut()) },
            ERR_INVALID
        );
    }

    #[test]
    fn a_missing_image_is_rejected_and_hashing_matches_sha2() {
        let scratch = TempDir::new("client-hash");
        let missing = scratch.path().join("parsecd-150-101a.dylib");
        let build = ClientBuild {
            version: "150".into(),
            build: "101a".into(),
        };
        assert!(install(&build, &missing).is_err());
        let file = scratch.path().join("owned.bin");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
