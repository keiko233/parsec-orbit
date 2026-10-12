//! Native UI patches - official Parsec Orbit plugin.
//!
//! The client configuration system is far more permissive than its user
//! interfaces. `encoder_bitrate` ("Bandwidth Limit") is an `int` with
//! `min 0` / `max 2147483647`, but the native (libmatoya) UI only offers the
//! built-in ladder `3, 5, 7, 10, 15, 20, 25, 30, 35, 40, 45, 50 Mbps`.
//!
//! This plugin patches the in-memory option table behind that dropdown; see
//! [`legacy`] and `README.md`.

pub mod legacy;

use parsec_orbit_plugin_sdk::{ParsecOrbitHost, LOG_INFO};

/// Bandwidth options appended after the built-in `3..50 mbps` ladder.
///
/// Keep in sync with the WebView plugin's `assets/bandwidth.js`.
pub const EXTRA_BANDWIDTH_MBPS: [i32; 10] = [60, 75, 100, 125, 150, 200, 250, 300, 400, 500];

/// `PARSEC_ORBIT_PATCHES=0` (or `off`/`false`) keeps the plugin idle. The switch
/// predates the plugin split, when the patch was compiled into the loader, and
/// is kept so existing setups keep working.
fn enabled() -> bool {
    !matches!(
        std::env::var("PARSEC_ORBIT_PATCHES").ok().as_deref(),
        Some("0") | Some("off") | Some("false")
    )
}

/// Copy of the host `log` callback that can be moved to a worker thread.
///
/// A function pointer is `Send + Sync`, so unlike the host table itself this
/// needs no `unsafe impl`.
#[derive(Clone, Copy)]
struct Log(unsafe extern "system" fn(u32, *const u8, usize));

impl Log {
    fn info(self, message: &str) {
        // Safety: the loader's host table and its log callback live for the
        // whole lifetime of the process, and the loader only reads `message`.
        unsafe { (self.0)(LOG_INFO, message.as_ptr(), message.len()) }
    }
}

unsafe extern "system" fn init(host: *const ParsecOrbitHost) -> i32 {
    if host.is_null() {
        return 1;
    }
    let Some(log) = (unsafe { &*host }).log else {
        return 1;
    };
    let log = Log(log);

    log.info(&format!(
        "native ui patches {} starting",
        env!("CARGO_PKG_VERSION")
    ));

    if !enabled() {
        log.info("PARSEC_ORBIT_PATCHES is set, the native ui plugin stays idle");
        return 0;
    }

    // The table only exists once the client has loaded its UI module, and the
    // signature scan waits for it; never do that on the loader's worker thread.
    std::thread::spawn(move || legacy::extend_bandwidth(|message| log.info(message)));

    0
}

parsec_orbit_plugin_sdk::parsec_orbit_plugin! {
    id = "com.parsec-orbit.native",
    name = "Native UI Patches",
    version = "0.1.0",
    author = "Parsec Orbit contributors",
    description = "Adds bandwidth limits above 50 Mbps to the native (legacy) UI",
    license = "MIT",
    init = init,
}
