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
    enabled_with(&env)
}

fn enabled_with(env: &dyn Fn(&str) -> Option<String>) -> bool {
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
    devtools_port_with(&env)
}

fn devtools_port_with(env: &dyn Fn(&str) -> Option<String>) -> Option<u16> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::CStr;
    use std::sync::Mutex;

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        // Same contract as `env`: an empty value counts as unset.
        move |name| vars.get(name).cloned().filter(|value| !value.is_empty())
    }

    // ---- enabled ------------------------------------------------------------

    #[test]
    fn plugin_is_enabled_unless_patches_are_switched_off() {
        assert!(enabled_with(&lookup(&[])));
        assert!(enabled_with(&lookup(&[("PARSEC_ORBIT_PATCHES", "")])));
        assert!(enabled_with(&lookup(&[("PARSEC_ORBIT_PATCHES", "1")])));
        assert!(enabled_with(&lookup(&[("PARSEC_ORBIT_PATCHES", "OFF")])));
        for off in ["0", "off", "false"] {
            assert!(
                !enabled_with(&lookup(&[("PARSEC_ORBIT_PATCHES", off)])),
                "{off}"
            );
        }
    }

    // ---- devtools_port ---------------------------------------------------------

    fn port(vars: &[(&str, &str)]) -> Option<u16> {
        devtools_port_with(&lookup(vars))
    }

    #[test]
    fn port_defaults_to_the_loader_default() {
        assert_eq!(port(&[]), Some(9222));
        assert_eq!(port(&[("PARSEC_ORBIT_DEVTOOLS_PORT", "")]), Some(9222));
    }

    #[test]
    fn port_follows_the_configured_port_variable() {
        assert_eq!(port(&[("PARSEC_ORBIT_DEVTOOLS_PORT", "9333")]), Some(9333));
        // Unparsable values fall back rather than switching the patch off.
        assert_eq!(port(&[("PARSEC_ORBIT_DEVTOOLS_PORT", "nope")]), Some(9222));
        assert_eq!(port(&[("PARSEC_ORBIT_DEVTOOLS_PORT", "70000")]), Some(9222));
    }

    #[test]
    fn port_is_none_when_devtools_are_switched_off() {
        for off in ["0", "off", "false"] {
            assert_eq!(port(&[("PARSEC_ORBIT_DEVTOOLS", off)]), None, "{off}");
            assert_eq!(
                port(&[
                    ("PARSEC_ORBIT_DEVTOOLS", off),
                    (
                        "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
                        "--remote-debugging-port=9300"
                    ),
                ]),
                None,
                "the off switch wins over the browser arguments ({off})"
            );
        }
        assert_eq!(port(&[("PARSEC_ORBIT_DEVTOOLS", "1")]), Some(9222));
    }

    #[test]
    fn port_is_read_back_from_the_browser_arguments_the_loader_set() {
        let arguments = "--remote-debugging-port=9300 --remote-allow-origins=*";
        assert_eq!(
            port(&[("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", arguments)]),
            Some(9300)
        );
        // The arguments are authoritative even if the port variable differs.
        assert_eq!(
            port(&[
                ("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", arguments),
                ("PARSEC_ORBIT_DEVTOOLS_PORT", "9444"),
            ]),
            Some(9300)
        );
        assert_eq!(
            port(&[(
                "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
                "--foo --remote-allow-origins=* --remote-debugging-port=1234 --bar"
            )]),
            Some(1234)
        );
    }

    #[test]
    fn port_is_none_when_the_user_brought_their_own_browser_arguments() {
        // The variable exists but names no debugging port: the endpoint is not
        // ours, so the plugin must not guess one.
        for arguments in [
            "--user-flag",
            "--remote-debugging-port=abc",
            "--remote-debugging-port=",
            "--remote-debugging-port=99999",
            "--remote-debugging-pipe",
        ] {
            assert_eq!(
                port(&[
                    ("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", arguments),
                    ("PARSEC_ORBIT_DEVTOOLS_PORT", "9444"),
                ]),
                None,
                "{arguments}"
            );
        }
    }

    // ---- init ----------------------------------------------------------------

    static LOGGED: Mutex<Vec<(u32, String)>> = Mutex::new(Vec::new());

    unsafe extern "system" fn record(level: u32, message: *const u8, len: usize) {
        let bytes = unsafe { std::slice::from_raw_parts(message, len) };
        LOGGED
            .lock()
            .unwrap()
            .push((level, String::from_utf8_lossy(bytes).into_owned()));
    }

    fn host(log: Option<unsafe extern "system" fn(u32, *const u8, usize)>) -> ParsecOrbitHost {
        ParsecOrbitHost {
            abi_version: parsec_orbit_plugin_sdk::ABI_VERSION,
            struct_size: std::mem::size_of::<ParsecOrbitHost>() as u32,
            log,
            query_interface: None,
            loader_version: std::ptr::null(),
            reserved: [0; 8],
        }
    }

    fn logged(text: &str) -> bool {
        LOGGED
            .lock()
            .unwrap()
            .iter()
            .any(|(_, message)| message.contains(text))
    }

    #[test]
    fn init_refuses_a_missing_host_or_a_host_without_logging() {
        assert_eq!(unsafe { init(std::ptr::null()) }, 1);
        assert_eq!(unsafe { init(&host(None)) }, 1);
    }

    /// Never reaches the code that connects to a DevTools port, so it cannot
    /// touch a client that is really running on this machine. The connecting
    /// path is covered against a fake endpoint in `tests/` and in the e2e
    /// crate. The only test that mutates the environment.
    #[test]
    fn init_stays_idle_or_warns_without_ever_spawning_the_injector() {
        let host = host(Some(record));

        std::env::set_var("PARSEC_ORBIT_PATCHES", "0");
        assert_eq!(unsafe { init(&host) }, 0);
        assert!(logged("webview ui patches"));
        assert!(logged("stays idle"));
        std::env::remove_var("PARSEC_ORBIT_PATCHES");

        std::env::set_var("PARSEC_ORBIT_DEVTOOLS", "off");
        assert_eq!(unsafe { init(&host) }, 0);
        assert!(logged("no WebView2 DevTools endpoint"));
        let warned = LOGGED
            .lock()
            .unwrap()
            .iter()
            .any(|(level, message)| *level == LOG_WARN && message.contains("no WebView2"));
        assert!(warned, "the missing endpoint is a warning");
        std::env::remove_var("PARSEC_ORBIT_DEVTOOLS");
    }

    // ---- descriptor ---------------------------------------------------------------

    fn manifest_value(key: &str) -> String {
        include_str!("../plugin.toml")
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once('=')?;
                (name.trim() == key).then(|| value.trim().trim_matches('"').to_string())
            })
            .unwrap_or_else(|| panic!("plugin.toml has no `{key}`"))
    }

    fn text(pointer: *const u8) -> String {
        unsafe { CStr::from_ptr(pointer as *const _) }
            .to_str()
            .unwrap()
            .to_string()
    }

    /// The loader prefers the manifest for de-duplication but logs the
    /// descriptor, so the two have to describe the same plugin.
    #[test]
    fn descriptor_and_manifest_agree() {
        let desc = unsafe { &*ParsecOrbitPluginQuery(parsec_orbit_plugin_sdk::ABI_VERSION) };
        assert_eq!(desc.abi_version, parsec_orbit_plugin_sdk::ABI_VERSION);
        assert_eq!(text(desc.id), manifest_value("id"));
        assert_eq!(text(desc.name), manifest_value("name"));
        assert_eq!(text(desc.version), manifest_value("version"));
        assert_eq!(text(desc.description), manifest_value("description"));
        assert_eq!(text(desc.license), manifest_value("license"));
        assert_eq!(text(desc.author), manifest_value("author"));
        assert_eq!(manifest_value("kind"), "dll");
        assert_eq!(manifest_value("api"), desc.abi_version.to_string());
        assert_eq!(manifest_value("entry"), "parsec_orbit_plugin_webview.dll");
    }
}
