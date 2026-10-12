//! WebView bridge - official Parsec Orbit plugin.
//!
//! Provides the `parsec-orbit.webview` interface: scripts that stay installed
//! in the client's WebView2 UI, evaluation, and messages in both directions.
//! It drives the DevTools endpoint the loader opens (see [`devtools`]) and
//! keeps the session alive, attaching again and replaying the scripts when
//! the page is recreated. Plugins - native ones through
//! `WebViewClient`, JavaScript ones through the runtime - use the interface
//! instead of carrying an HTTP/WebSocket client of their own.

pub mod bridge;
pub mod devtools;
pub mod service;

use parsec_orbit_plugin_sdk_std::prelude::*;
use parsec_orbit_plugin_sdk_std::services::WebView;

pub use devtools::{inject_once, Injection};

/// Port the loader opens the WebView2 DevTools endpoint on by default.
const DEFAULT_DEVTOOLS_PORT: u16 = 9222;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
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

#[derive(Default)]
struct WebViewBridge;

impl Plugin for WebViewBridge {
    fn init(&mut self, cx: &Context) -> Result {
        info!(cx, "webview bridge {} starting", env!("CARGO_PKG_VERSION"));

        // The interface exists either way: without an endpoint every call
        // answers "not ready", which plugins already have to handle.
        cx.register::<WebView>(&service::WEBVIEW)?;

        match devtools_port() {
            Some(port) => {
                // The session polls until the endpoint exists and attaches
                // again when the page is recreated; it must not run on the
                // loader's worker thread, and it only starts once a plugin
                // asks for something.
                let cx = *cx;
                service::bridge().configure(
                    port,
                    cx.plugin_id(),
                    Box::new(move |level, message| cx.log(level, format_args!("{message}"))),
                );
            }
            None => warn!(
                cx,
                "no WebView2 DevTools endpoint (disabled or custom browser arguments), the web UI is left untouched"
            ),
        }
        Ok(())
    }
}

parsec_orbit_plugin_sdk_std::declare_plugin! {
    id = "com.parsec-orbit.webview-bridge",
    name = "WebView Bridge",
    version = "0.1.0",
    author = "Parsec Orbit contributors",
    description = "Provides parsec-orbit.webview: scripts, evaluation and messages for the WebView2 UI",
    license = "MIT",
    plugin = WebViewBridge,
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_plugin_sdk_std::ParsecOrbitWebViewV1;
    use std::collections::HashMap;

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        // Same contract as `env`: an empty value counts as unset.
        move |name| vars.get(name).cloned().filter(|value| !value.is_empty())
    }

    #[test]
    fn the_default_port_applies_without_any_configuration() {
        assert_eq!(devtools_port_with(&lookup(&[])), Some(9222));
        assert_eq!(
            devtools_port_with(&lookup(&[("PARSEC_ORBIT_DEVTOOLS_PORT", "9333")])),
            Some(9333)
        );
        assert_eq!(
            devtools_port_with(&lookup(&[("PARSEC_ORBIT_DEVTOOLS_PORT", "junk")])),
            Some(9222)
        );
    }

    #[test]
    fn the_endpoint_can_be_switched_off() {
        for off in ["0", "off", "false"] {
            assert_eq!(
                devtools_port_with(&lookup(&[("PARSEC_ORBIT_DEVTOOLS", off)])),
                None,
                "{off}"
            );
        }
    }

    #[test]
    fn the_browser_arguments_decide_when_they_are_set() {
        let port = |arguments: &str| {
            devtools_port_with(&lookup(&[
                ("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", arguments),
                ("PARSEC_ORBIT_DEVTOOLS_PORT", "9999"),
            ]))
        };
        assert_eq!(
            port("--remote-debugging-port=9333 --remote-allow-origins=*"),
            Some(9333)
        );
        assert_eq!(
            port("--foo --remote-debugging-port=9444"),
            Some(9444),
            "wherever it is in the list"
        );
        assert_eq!(port("--foo"), None, "the user's own arguments, no endpoint");
        assert_eq!(port("--remote-debugging-port=nope"), None);
    }

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
        unsafe { std::ffi::CStr::from_ptr(pointer as *const _) }
            .to_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn descriptor_and_manifest_agree() {
        let desc = unsafe { &*ParsecOrbitPluginQuery(parsec_orbit_plugin_sdk_std::ABI_VERSION) };
        assert_eq!(text(desc.id), manifest_value("id"));
        assert_eq!(text(desc.name), manifest_value("name"));
        assert_eq!(text(desc.version), manifest_value("version"));
        assert_eq!(text(desc.description), manifest_value("description"));
        assert_eq!(text(desc.license), manifest_value("license"));
        assert_eq!(text(desc.author), manifest_value("author"));
        assert_eq!(manifest_value("kind"), "dll");
        assert_eq!(
            manifest_value("entry"),
            "parsec_orbit_plugin_webview_bridge.dll"
        );
        assert_eq!(text(desc.id), "com.parsec-orbit.webview-bridge");
    }

    #[test]
    fn the_table_is_a_version_1_webview_interface() {
        assert_eq!(service::WEBVIEW.header.major(), 1);
        assert_eq!(
            service::WEBVIEW.header.size as usize,
            std::mem::size_of::<ParsecOrbitWebViewV1>()
        );
        assert!(service::WEBVIEW.is_ready.is_some() && service::WEBVIEW.post.is_some());
    }
}
