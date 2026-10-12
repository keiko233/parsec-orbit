//! WebView UI patches - official Parsec Orbit plugin.
//!
//! The client configuration system is far more permissive than its user
//! interfaces. `encoder_bitrate` ("Bandwidth Limit") is an `int` with
//! `min 0` / `max 2147483647`, but the WebView UI only offers the built-in
//! ladder `3, 5, 7, 10, 15, 20, 25, 30, 35, 40, 45, 50 Mbps`.
//!
//! This plugin delivers [`webview::BITRATE_SCRIPT`] (built from
//! `assets/bandwidth.js`) over the DevTools endpoint the loader opens and
//! widens the dropdown to `..500 Mbps`. The loader stays responsible for the
//! endpoint itself, so a missing plugin simply leaves the web UI untouched.

pub mod webview;

use parsec_orbit_plugin_sdk::{ParsecOrbitHost, LOG_INFO, LOG_WARN};

/// Port the loader opens the WebView2 DevTools endpoint on by default.
const DEFAULT_DEVTOOLS_PORT: u16 = 9222;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// `PARSEC_ORBIT_PATCHES=0` (or `off`/`false`) keeps the plugin idle. The switch
/// predates the plugin split, when the patch was compiled into the loader, and
/// is kept so existing setups keep working.
fn enabled() -> bool {
    !matches!(
        env("PARSEC_ORBIT_PATCHES").as_deref(),
        Some("0") | Some("off") | Some("false")
    )
}

/// Port of the DevTools endpoint the loader opened, if any.
///
/// The loader puts `--remote-debugging-port=<port>` into
/// `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`. Parsing that back (instead of just
/// trusting the environment) keeps the plugin honest when the user supplied
/// their own browser arguments or disabled the endpoint.
fn devtools_port() -> Option<u16> {
    if matches!(
        env("PARSEC_ORBIT_DEVTOOLS").as_deref(),
        Some("0") | Some("off") | Some("false")
    ) {
        return None;
    }

    if let Some(arguments) = env("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS") {
        // The variable exists: the port has to be in it, otherwise the
        // endpoint is not ours to use.
        return arguments
            .split_whitespace()
            .find_map(|token| token.strip_prefix("--remote-debugging-port="))
            .and_then(|value| value.parse().ok());
    }

    env("PARSEC_ORBIT_DEVTOOLS_PORT")
        .and_then(|value| value.parse().ok())
        .or(Some(DEFAULT_DEVTOOLS_PORT))
}

/// Copy of the host `log` callback that can be moved to a worker thread.
///
/// A function pointer is `Send + Sync`, so unlike the host table itself this
/// needs no `unsafe impl`.
#[derive(Clone, Copy)]
struct Log(unsafe extern "system" fn(u32, *const u8, usize));

impl Log {
    fn write(self, level: u32, message: &str) {
        // Safety: the loader's host table and its log callback live for the
        // whole lifetime of the process, and the loader only reads `message`.
        unsafe { (self.0)(level, message.as_ptr(), message.len()) }
    }

    fn info(self, message: &str) {
        self.write(LOG_INFO, message);
    }

    fn warn(self, message: &str) {
        self.write(LOG_WARN, message);
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
        "webview ui patches {} starting",
        env!("CARGO_PKG_VERSION")
    ));

    if !enabled() {
        log.info("PARSEC_ORBIT_PATCHES is set, the webview plugin stays idle");
        return 0;
    }

    match devtools_port() {
        Some(port) => {
            // The loop polls until the endpoint exists and reconnects when the
            // page is recreated; it must not run on the loader's worker thread.
            std::thread::spawn(move || {
                webview::inject_script_forever(port, |message| log.info(message));
            });
        }
        None => log.warn(
            "no WebView2 DevTools endpoint (disabled or custom browser arguments), the web UI is left untouched",
        ),
    }

    0
}

parsec_orbit_plugin_sdk::parsec_orbit_plugin! {
    id = "com.parsec-orbit.webview",
    name = "WebView UI Patches",
    version = "0.1.0",
    author = "Parsec Orbit contributors",
    description = "Adds bandwidth limits above 50 Mbps to the WebView UI",
    license = "MIT",
    init = init,
}
