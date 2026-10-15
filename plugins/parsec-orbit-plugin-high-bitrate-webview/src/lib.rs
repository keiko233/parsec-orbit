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
//!
//! The plugin is `#![no_std]` and installs the SDK's freestanding runtime, as
//! `examples/parsec-orbit-plugin-hello-nostd` does. It reads its one switch
//! from the environment without `std`, see the `environment` module.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use core::fmt::Write;
use parsec_orbit_plugin_sdk::prelude::*;
use parsec_orbit_plugin_sdk::services::{ConfigClient, WebViewClient};

parsec_orbit_plugin_sdk::freestanding!();

/// Injected into the web UI; see `assets/bandwidth.js`.
pub const BITRATE_SCRIPT: &str = include_str!("../assets/bandwidth.js");

const CONFIG_OWNER: &str = "com.parsec-orbit.native";
const CONFIG_KEY: &str = "extra_bandwidth_mbps";
const MAX_EXTRA_OPTIONS: usize = 128;

fn normalize_extras(mut values: alloc::vec::Vec<i32>) -> Option<alloc::vec::Vec<i32>> {
    if values.len() > MAX_EXTRA_OPTIONS || values.iter().any(|mbps| *mbps <= 50) {
        return None;
    }
    values.sort_unstable();
    values.dedup();
    Some(values)
}

fn parse_extras(json: &str) -> Option<alloc::vec::Vec<i32>> {
    let bytes = json.as_bytes();
    let mut cursor = 0;
    let mut values = alloc::vec::Vec::new();
    let skip_space = |cursor: &mut usize| {
        while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
            *cursor += 1;
        }
    };
    skip_space(&mut cursor);
    if bytes.get(cursor) != Some(&b'[') {
        return None;
    }
    cursor += 1;
    loop {
        skip_space(&mut cursor);
        if bytes.get(cursor) == Some(&b']') {
            cursor += 1;
            break;
        }
        let start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if start == cursor {
            return None;
        }
        if bytes[start] == b'0' && cursor - start > 1 {
            return None;
        }
        let value = json[start..cursor].parse::<i32>().ok()?;
        values.push(value);
        if values.len() > MAX_EXTRA_OPTIONS {
            return None;
        }
        skip_space(&mut cursor);
        match bytes.get(cursor) {
            Some(b',') => {
                cursor += 1;
                let mut next = cursor;
                skip_space(&mut next);
                if bytes.get(next) == Some(&b']') {
                    return None;
                }
            }
            Some(b']') => {
                cursor += 1;
                break;
            }
            _ => return None,
        }
    }
    skip_space(&mut cursor);
    if cursor != bytes.len() {
        return None;
    }
    normalize_extras(values)
}

fn default_extras() -> alloc::vec::Vec<i32> {
    parse_extras(include_str!(
        "../../parsec-orbit-plugin-high-bitrate-native/assets/default-bandwidth-mbps.json"
    ))
    .expect("shared default bitrate options are valid")
}

fn read_extras(
    config: Option<ConfigClient>,
) -> core::result::Result<alloc::vec::Vec<i32>, &'static str> {
    let mut json = None;
    let config = config.ok_or("config service is unavailable")?;
    if !config
        .for_owner(CONFIG_OWNER)
        .get(CONFIG_KEY, |value| json = Some(String::from(value)))
        .map_err(|_| "config service read failed")?
    {
        return Ok(default_extras());
    }
    json.and_then(|value| parse_extras(&value))
        .ok_or("saved bandwidth options are invalid")
}

fn script_with_extras(extras: &[i32]) -> String {
    let marker = "const EXTRA_MBPS = [";
    let start = BITRATE_SCRIPT.find(marker).expect("bandwidth list marker") + marker.len();
    let end = start
        + BITRATE_SCRIPT[start..]
            .find("];\n")
            .expect("bandwidth list end");
    let mut script = String::with_capacity(BITRATE_SCRIPT.len() + extras.len() * 5);
    script.push_str(&BITRATE_SCRIPT[..start]);
    for (index, mbps) in extras.iter().enumerate() {
        if index != 0 {
            script.push_str(", ");
        }
        let _ = write!(script, "{mbps}");
    }
    script.push_str(&BITRATE_SCRIPT[end..]);
    script
}

/// Key of the script in the bridge. The bridge adds the plugin id.
const SCRIPT_KEY: &str = "bandwidth";

/// Reads one environment variable without `std`: `GetEnvironmentVariableW`
/// from kernel32 on Windows, libc `getenv` on macOS, and "unset" elsewhere.
mod environment {
    /// Bytes of a value kept on the stack. The switch words are at most five
    /// bytes long, so a longer value can never be one of them.
    const KEEP: usize = 8;
    /// Room for a variable name and its terminating NUL.
    const NAME_CAP: usize = 64;

    /// A value copied onto the stack: its length and its first `KEEP` bytes.
    #[derive(Clone, Copy, Default)]
    pub struct Value {
        len: usize,
        head: [u8; KEEP],
    }

    impl Value {
        /// Appends one byte of the value.
        pub fn push(&mut self, byte: u8) {
            if self.len < KEEP {
                self.head[self.len] = byte;
            }
            self.len += 1;
        }

        /// Whether the value is exactly `word`.
        pub fn is(&self, word: &str) -> bool {
            self.len == word.len() && self.head.starts_with(word.as_bytes())
        }
    }

    #[cfg(windows)]
    pub fn var(name: &str) -> Option<Value> {
        #[link(name = "kernel32", kind = "raw-dylib")]
        extern "system" {
            fn GetEnvironmentVariableW(name: *const u16, buffer: *mut u16, size: u32) -> u32;
        }

        // Names are short constants; the byte length bounds the UTF-16 length,
        // which leaves the NUL in place.
        assert!(name.len() < NAME_CAP);
        let mut key = [0u16; NAME_CAP];
        for (slot, unit) in key.iter_mut().zip(name.encode_utf16()) {
            *slot = unit;
        }
        let mut buffer = [0u16; NAME_CAP];
        // Safety: both buffers are valid for the sizes passed, and `key` is
        // NUL-terminated by its zeroed tail.
        let units = unsafe {
            GetEnvironmentVariableW(key.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32)
        } as usize;
        // 0 is "unset" or "empty"; both count as the default.
        if units == 0 {
            return None;
        }
        let mut value = Value::default();
        if units >= buffer.len() {
            // Longer than the buffer, which is more than any switch word: the
            // length alone decides it.
            value.len = units;
            return Some(value);
        }
        // A non-ASCII unit becomes a byte no switch word contains.
        for &unit in &buffer[..units] {
            value.push(u8::try_from(unit).unwrap_or(0xFF));
        }
        Some(value)
    }

    #[cfg(target_os = "macos")]
    pub fn var(name: &str) -> Option<Value> {
        use core::ffi::{c_char, CStr};

        #[link(name = "System")]
        extern "C" {
            fn getenv(name: *const c_char) -> *const c_char;
        }

        assert!(name.len() < NAME_CAP);
        let mut key = [0u8; NAME_CAP];
        key[..name.len()].copy_from_slice(name.as_bytes());
        // Safety: `key` is NUL-terminated by its zeroed tail; `getenv` returns
        // either null or a NUL-terminated string owned by the process.
        let pointer = unsafe { getenv(key.as_ptr().cast()) };
        if pointer.is_null() {
            return None;
        }
        let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
        let mut value = Value::default();
        for &byte in bytes {
            value.push(byte);
        }
        Some(value)
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    pub fn var(_name: &str) -> Option<Value> {
        None
    }
}

/// `PARSEC_ORBIT_PATCHES=0` (or `off`/`false`) keeps the plugin idle. The switch
/// predates the plugin split, when the patch was compiled into the loader, and
/// is kept so existing setups keep working. Unset or empty means enabled.
fn enabled() -> bool {
    enabled_with(&environment::var)
}

fn enabled_with(env: &dyn Fn(&str) -> Option<environment::Value>) -> bool {
    match env("PARSEC_ORBIT_PATCHES") {
        Some(value) => !(value.is("0") || value.is("off") || value.is("false")),
        None => true,
    }
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
        let extras = match read_extras(ConfigClient::new(cx)) {
            Ok(extras) => extras,
            Err(error) => {
                warn!(cx, "{error}; using the built-in bandwidth options");
                default_extras()
            }
        };
        let script = script_with_extras(&extras);
        match webview.add_script(SCRIPT_KEY, &script) {
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
    use super::environment::Value;
    use super::*;
    use parsec_orbit_plugin_sdk::{
        ParsecOrbitHost, ParsecOrbitInterfaceHeader, ParsecOrbitWebViewV1, ERR_INVALID, LOG_INFO,
        LOG_WARN,
    };
    use parsec_orbit_plugin_sdk_testing::MockHost;
    use std::collections::HashMap;
    use std::ffi::CStr;
    use std::sync::Mutex;

    fn value_of(text: &str) -> Value {
        let mut value = Value::default();
        for byte in text.bytes() {
            value.push(byte);
        }
        value
    }

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<Value> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        // Same contract as `environment::var`: an empty value counts as unset.
        move |name| {
            vars.get(name)
                .filter(|value| !value.is_empty())
                .map(|value| value_of(value))
        }
    }

    // ---- enabled ------------------------------------------------------------

    #[test]
    fn plugin_is_enabled_unless_patches_are_switched_off() {
        assert!(enabled_with(&lookup(&[])));
        assert!(enabled_with(&lookup(&[("PARSEC_ORBIT_PATCHES", "")])));
        assert!(enabled_with(&lookup(&[("PARSEC_ORBIT_PATCHES", "1")])));
        assert!(enabled_with(&lookup(&[("PARSEC_ORBIT_PATCHES", "OFF")])));
        // Only an exact match switches the plugin off.
        assert!(enabled_with(&lookup(&[("PARSEC_ORBIT_PATCHES", "0 ")])));
        assert!(enabled_with(&lookup(&[(
            "PARSEC_ORBIT_PATCHES",
            "falseXXXXXXXX"
        )])));
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
        assert_eq!(registered[0].1, script_with_extras(&default_extras()));
    }

    #[test]
    fn the_script_is_the_asset() {
        assert!(BITRATE_SCRIPT.contains("__orbitBitrateExtend"));
        assert!(BITRATE_SCRIPT.contains("const EXTRA_MBPS = ["));
    }

    #[test]
    fn custom_integer_ladder_is_validated_and_injected() {
        assert_eq!(
            parse_extras("[60, 85, 125]"),
            Some(alloc::vec![60, 85, 125])
        );
        assert_eq!(parse_extras("[125, 85, 85]"), Some(alloc::vec![85, 125]));
        for invalid in ["[50]", "[85.5]", "[-85]", "[85,]", "[085]", "[2147483648]"] {
            assert_eq!(parse_extras(invalid), None, "{invalid}");
        }
        let script = script_with_extras(&[60, 85, 125]);
        assert!(script.contains("const EXTRA_MBPS = [60, 85, 125];"));
        assert!(!script.contains("[60, 75, 100, 125, 150"));
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
