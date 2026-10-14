//! WebView UI patches - official Parsec Orbit plugin.
//!
//! The client configuration system is far more permissive than its user
//! interfaces. `encoder_bitrate` ("Bandwidth Limit") is an `int` with
//! `min 0` / `max 2147483647`, but the WebView UI only offers the built-in
//! ladder `3, 5, 7, 10, 15, 20, 25, 30, 35, 40, 45, 50 Mbps`.
//!
//! This plugin hands [`BITRATE_SCRIPT`] (built from `assets/bandwidth.js`) to
//! the `parsec-orbit.webview` interface of the webview-bridge plugin, which
//! keeps it installed in the client's WebView2 UI - in the current document and
//! in every new one - and widens the dropdown to `..500 Mbps`. Without the
//! bridge, or without a DevTools endpoint, the web UI is left untouched.

use parsec_orbit_plugin_sdk::prelude::*;
use parsec_orbit_plugin_sdk::services::WebViewClient;

/// Injected into the web UI; see `assets/bandwidth.js`.
pub const BITRATE_SCRIPT: &str = include_str!("../assets/bandwidth.js");

/// Key of the script in the bridge. The bridge adds the plugin id.
const SCRIPT_KEY: &str = "bandwidth";

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

#[derive(Default)]
struct Webview;

impl Plugin for Webview {
    fn init(&mut self, cx: &Context) -> Result {
        info!(
            cx,
            "webview ui patches {} starting",
            env!("CARGO_PKG_VERSION")
        );

        if !enabled() {
            info!(
                cx,
                "PARSEC_ORBIT_PATCHES is set, the webview plugin stays idle"
            );
            return Ok(());
        }

        let Some(webview) = WebViewClient::new(cx) else {
            warn!(
                cx,
                "parsec-orbit.webview is not available (is the webview bridge installed?), the web UI is left untouched"
            );
            return Ok(());
        };

        // The bridge keeps the script installed: it runs in the current
        // document and in every new one, and is replayed if the page is
        // recreated. Nothing is waited for here.
        match webview.add_script(SCRIPT_KEY, BITRATE_SCRIPT) {
            Ok(id) => {
                info!(
                    cx,
                    "webview bandwidth patch handed to the bridge (script {id})"
                );
                Ok(())
            }
            Err(error) => {
                error!(cx, "the bridge refused the webview patch: {error}");
                Err(Error::with_message(
                    error.code(),
                    "could not hand the patch to the webview bridge",
                ))
            }
        }
    }
}

parsec_orbit_plugin_sdk::declare_plugin! {
    id = "com.parsec-orbit.webview",
    name = "WebView UI Patches",
    version = "0.1.1",
    author = "Parsec Orbit contributors",
    description = "Adds bandwidth limits above 50 Mbps to the WebView UI",
    license = "MIT",
    plugin = Webview,
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_plugin_sdk::{
        ParsecOrbitHost, ParsecOrbitInterfaceHeader, ParsecOrbitWebViewV1, ERR_INVALID, LOG_INFO,
        LOG_WARN,
    };
    use parsec_orbit_plugin_sdk_testing::MockHost;
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

    // ---- init ----------------------------------------------------------------

    #[test]
    fn init_refuses_a_missing_or_too_small_host_table() {
        assert_eq!(
            unsafe { ParsecOrbitPluginInit(std::ptr::null()) },
            ERR_INVALID
        );
        let mut tiny = ParsecOrbitHost::blank();
        tiny.struct_size = 8;
        assert_eq!(unsafe { ParsecOrbitPluginInit(&tiny) }, ERR_INVALID);
    }

    static REGISTERED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

    unsafe extern "system" fn add_script(
        key: *const u8,
        key_len: usize,
        script: *const u8,
        script_len: usize,
    ) -> i64 {
        let text = |pointer: *const u8, len: usize| {
            String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(pointer, len) })
                .into_owned()
        };
        REGISTERED
            .lock()
            .unwrap()
            .push((text(key, key_len), text(script, script_len)));
        5
    }

    static BRIDGE: ParsecOrbitWebViewV1 = ParsecOrbitWebViewV1 {
        header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitWebViewV1>(), 1, 0),
        is_ready: None,
        add_script: Some(add_script),
        remove_script: None,
        evaluate: None,
        subscribe: None,
        unsubscribe: None,
        post: None,
    };

    /// The only test that mutates the environment.
    #[test]
    fn init_hands_the_script_to_the_bridge_or_leaves_the_ui_alone() {
        let host = MockHost::new("com.parsec-orbit.webview");

        // Switched off: nothing is looked up, nothing is registered.
        std::env::set_var("PARSEC_ORBIT_PATCHES", "0");
        assert!(host.init::<Webview>().1.is_ok());
        assert!(host.logged("webview ui patches"));
        assert!(host.logged("stays idle"));
        std::env::remove_var("PARSEC_ORBIT_PATCHES");

        // No bridge: a warning, still a success - the UI is simply untouched.
        host.clear_logs();
        assert!(host.init::<Webview>().1.is_ok());
        assert!(host
            .logs()
            .iter()
            .any(|line| line.level == LOG_WARN && line.message.contains("webview bridge")));
        assert!(REGISTERED.lock().unwrap().is_empty());

        // With the bridge the script is registered under the plugin's key.
        host.provide("parsec-orbit.webview", &BRIDGE.header);
        host.clear_logs();
        assert!(host.init::<Webview>().1.is_ok());
        assert!(host
            .logs()
            .iter()
            .any(|line| line.level == LOG_INFO && line.message.contains("(script 5)")));
        let registered = REGISTERED.lock().unwrap();
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].0, "com.parsec-orbit.webview/bandwidth");
        assert_eq!(registered[0].1, BITRATE_SCRIPT);
    }

    #[test]
    fn the_script_is_the_asset() {
        assert!(BITRATE_SCRIPT.contains("__orbitBitrateExtend"));
        assert!(BITRATE_SCRIPT.contains("const EXTRA_MBPS = ["));
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
        assert_eq!(
            manifest_value("entry"),
            "parsec_orbit_plugin_high_bitrate_webview.dll"
        );
    }

    /// Without the bridge the plugin does nothing, so the loader has to load
    /// the bridge first.
    #[test]
    fn the_manifest_requires_the_bridge() {
        let manifest = include_str!("../plugin.toml");
        assert!(
            manifest.contains("requires = [\"com.parsec-orbit.webview-bridge\"]"),
            "{manifest}"
        );
    }
}
