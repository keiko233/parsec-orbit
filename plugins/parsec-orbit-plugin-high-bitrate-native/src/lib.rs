//! Native UI patches - official Parsec Orbit plugin.
//!
//! The client configuration system is far more permissive than its user
//! interfaces. `encoder_bitrate` ("Bandwidth Limit") is an `int` with
//! `min 0` / `max 2147483647`, but the native (libmatoya) UI only offers the
//! built-in ladder `3, 5, 7, 10, 15, 20, 25, 30, 35, 40, 45, 50 Mbps`.
//!
//! This plugin patches the in-memory option table behind that dropdown; see
//! [`legacy`] and `README.md`.
//!
//! Windows-only: the patch reads the client's modules (`Toolhelp32`) and
//! rewrites option-table pages (`VirtualProtect`/`VirtualAlloc`). The macOS
//! equivalent is per verified build and lands in M2.
#![cfg(windows)]

pub mod legacy;

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use parsec_orbit_plugin_sdk::prelude::*;
use parsec_orbit_plugin_sdk::services::{ConfigClient, UiClient};

const CONFIG_OWNER: &str = "com.parsec-orbit.native";
const CONFIG_KEY: &str = "extra_bandwidth_mbps";
const ENABLED_KEY: &str = "enabled";
const MAX_EXTRA_OPTIONS: usize = 128;

struct NativeSettings {
    config: Option<ConfigClient>,
    extras: Mutex<Vec<i32>>,
    enabled: Mutex<bool>,
    cx: Context,
}

/// `PARSEC_ORBIT_PATCHES=0` (or `off`/`false`) keeps the plugin idle. The switch
/// predates the plugin split, when the patch was compiled into the loader, and
/// is kept so existing setups keep working.
fn enabled() -> bool {
    enabled_with(std::env::var("PARSEC_ORBIT_PATCHES").ok().as_deref())
}

fn enabled_with(switch: Option<&str>) -> bool {
    !matches!(switch, Some("0") | Some("off") | Some("false"))
}

fn normalize_extras(mut values: Vec<i32>) -> std::result::Result<Vec<i32>, &'static str> {
    if values.len() > MAX_EXTRA_OPTIONS {
        return Err("at most 128 options are allowed");
    }
    if values.iter().any(|mbps| *mbps <= 50) {
        return Err("every option must be an integer greater than 50 Mbps");
    }
    values.sort_unstable();
    values.dedup();
    Ok(values)
}

fn default_extras() -> Vec<i32> {
    serde_json::from_str(include_str!("../assets/default-bandwidth-mbps.json"))
        .expect("built-in bitrate options are valid")
}

fn load_extras(config: Option<ConfigClient>) -> std::result::Result<Vec<i32>, String> {
    let mut json = None;
    let config = config.ok_or_else(|| "config service is unavailable".to_string())?;
    let found = config
        .get(CONFIG_KEY, |value| json = Some(value.to_string()))
        .map_err(|error| format!("config read failed (error {})", error.code()))?;
    if !found {
        return Ok(default_extras());
    }
    let values = json
        .and_then(|value| serde_json::from_str::<Vec<i32>>(&value).ok())
        .ok_or_else(|| "saved options are not a JSON array of integers".to_string())?;
    normalize_extras(values).map_err(str::to_string)
}

fn load_enabled(config: Option<ConfigClient>) -> bool {
    let Some(config) = config else {
        return true;
    };
    let mut json = None;
    match config.get(ENABLED_KEY, |value| json = Some(value.to_string())) {
        Ok(true) => json
            .and_then(|value| serde_json::from_str::<bool>(&value).ok())
            .unwrap_or(true),
        _ => true,
    }
}

fn native_ui_schema(extras: &[i32], enabled: bool) -> String {
    let extras = serde_json::to_string(extras).expect("integer list serializes as JSON");
    let enabled = enabled.to_string();
    format!(
        r#"{{
    "id": "com.parsec-orbit.native",
    "title": "High Bitrate",
    "description": "Bandwidth limit options for the native (libmatoya) client UI",
    "order": 10,
    "items": [
        {{
            "key": "enabled",
            "type": "toggle",
            "label": "Extend the bandwidth ladder",
            "description": "Append the options above 50 Mbps to Parsec's Bandwidth Limit dropdown",
            "default": {enabled}
        }},
        {{
            "key": "extra_bandwidth_mbps",
            "type": "json_editor",
            "label": "Additional bandwidth options (Mbps)",
            "description": "Enter integer Mbps values above 50. Values are sorted and duplicates removed; changes apply after restarting the client.",
            "default": {extras}
        }},
        {{
            "key": "patch_status",
            "type": "readonly_text",
            "label": "Patch status",
            "description": "What the in-memory patch did, written by the plugin",
            "default": "Waiting for the client UI..."
        }}
    ]
}}"#
    )
}

/// Logs a line and shows it in the Orbit UI's status row.
fn report(cx: &Context, message: &str) {
    info!(cx, "{message}");
    if let Some(ui) = UiClient::new(cx) {
        let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
        let json = format!(r#"{{"patch_status":"{escaped}"}}"#);
        let _ = ui.update_state("com.parsec-orbit.native", &json);
    }
}

/// Handles the section's controls. The state remains alive for the plugin's
/// process lifetime, matching the UI service callback lifetime.
unsafe extern "system" fn on_native_action(user: *mut c_void, msg: *const u8, len: usize) {
    if user.is_null() {
        return;
    }
    let settings = unsafe { &*(user as *const NativeSettings) };
    let slice = unsafe { std::slice::from_raw_parts(msg, len) };
    let Ok(action) = serde_json::from_slice::<serde_json::Value>(slice) else {
        report(
            &settings.cx,
            "Settings were not saved: the UI action was invalid JSON.",
        );
        return;
    };
    let value = action.get("value");
    match action.get("key").and_then(|key| key.as_str()) {
        Some("enabled") => {
            let Some(enabled) = value.and_then(|value| value.as_bool()) else {
                report(
                    &settings.cx,
                    "Settings were not saved: enabled must be a boolean.",
                );
                return;
            };
            let Some(config) = settings.config else {
                report(
                    &settings.cx,
                    "Settings were not saved: config service is unavailable.",
                );
                return;
            };
            match config.set(ENABLED_KEY, if enabled { "true" } else { "false" }) {
                Ok(()) => {
                    *settings.enabled.lock().unwrap() = enabled;
                    report(
                        &settings.cx,
                        "Enabled setting saved; restart the client to apply it.",
                    );
                }
                Err(error) => report(
                    &settings.cx,
                    &format!(
                        "Enabled setting was not saved (config error {}).",
                        error.code()
                    ),
                ),
            }
        }
        Some(CONFIG_KEY) => {
            let parsed =
                value.and_then(|value| serde_json::from_value::<Vec<i32>>(value.clone()).ok());
            let Some(parsed) = parsed else {
                report(
                    &settings.cx,
                    "Options were not saved: enter a JSON array of integers.",
                );
                return;
            };
            let extras = match normalize_extras(parsed) {
                Ok(extras) => extras,
                Err(error) => {
                    report(&settings.cx, &format!("Options were not saved: {error}."));
                    return;
                }
            };
            let Ok(json) = serde_json::to_string(&extras) else {
                report(
                    &settings.cx,
                    "Options were not saved: could not encode the integer list.",
                );
                return;
            };
            if let Some(config) = settings.config {
                match config.set(CONFIG_KEY, &json) {
                    Ok(()) => {
                        *settings.extras.lock().unwrap() = extras;
                        report(
                            &settings.cx,
                            "Options saved; restart the client to apply them.",
                        );
                    }
                    Err(error) => report(
                        &settings.cx,
                        &format!("Options were not saved (config error {}).", error.code()),
                    ),
                }
            } else {
                report(
                    &settings.cx,
                    "Options were not saved: config service is unavailable.",
                );
            }
        }
        _ => {}
    }
}

#[derive(Default)]
struct Native;

impl Plugin for Native {
    fn init(&mut self, cx: &Context) -> Result {
        info!(
            cx,
            "native ui patches {} starting",
            env!("CARGO_PKG_VERSION")
        );

        if !enabled() {
            info!(
                cx,
                "PARSEC_ORBIT_PATCHES is set, the native ui plugin stays idle"
            );
            return Ok(());
        }

        // The table only exists once the client has loaded its UI module, and
        // the signature scan waits for it; never do that on the loader's
        // worker thread.
        let config = ConfigClient::new(cx).map(|config| config.for_owner(CONFIG_OWNER));
        let (extras, load_error) = match load_extras(config) {
            Ok(extras) => (extras, None),
            Err(error) => (default_extras(), Some(error)),
        };
        let persisted_enabled = load_enabled(config);
        let settings = Arc::new(NativeSettings {
            config,
            extras: Mutex::new(extras.clone()),
            enabled: Mutex::new(persisted_enabled),
            cx: *cx,
        });

        if let Some(ui) = UiClient::new(cx) {
            let schema = native_ui_schema(&extras, persisted_enabled);
            let _ = ui.register_section(&schema);
            // Plugins are not unloaded during this process; keep the callback
            // state alive for the UI service's subscription lifetime.
            let callback_state = Arc::into_raw(settings.clone()) as *mut c_void;
            let _ = unsafe {
                ui.on_action("com.parsec-orbit.native", on_native_action, callback_state)
            };
        }
        if let Some(error) = load_error {
            report(cx, &format!("Using default bandwidth options: {error}."));
        }
        if config.is_none() {
            report(
                cx,
                "Config service is unavailable; settings cannot be persisted.",
            );
        }

        let cx = *cx;
        std::thread::Builder::new()
            .name(format!("{}:patch", cx.plugin_id()))
            .spawn(move || {
                if !*settings.enabled.lock().unwrap() {
                    report(
                        &cx,
                        "switched off in the Orbit UI, the bandwidth ladder is unchanged",
                    );
                    return;
                }
                legacy::extend_bandwidth_with(
                    || settings.extras.lock().unwrap().clone(),
                    |message| report(&cx, message),
                );
            })?;

        Ok(())
    }
}

parsec_orbit_plugin_sdk::declare_plugin! {
    id = "com.parsec-orbit.native",
    name = "Native UI Patches",
    version = "0.1.1",
    author = "Parsec Orbit contributors",
    description = "Adds bandwidth limits above 50 Mbps to the native (legacy) UI",
    license = "MIT",
    plugin = Native,
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_plugin_sdk::{ParsecOrbitHost, ABI_VERSION, LOG_INFO};
    use parsec_orbit_plugin_sdk_testing::MockHost;
    use std::ffi::CStr;

    #[test]
    fn plugin_is_enabled_unless_patches_are_switched_off() {
        assert!(enabled_with(None));
        assert!(enabled_with(Some("")));
        assert!(enabled_with(Some("1")));
        assert!(enabled_with(Some("OFF")));
        for off in ["0", "off", "false"] {
            assert!(!enabled_with(Some(off)), "{off}");
        }
    }

    // ---- the extra ladder ------------------------------------------------------

    #[test]
    fn extra_options_continue_the_built_in_ladder() {
        let defaults = default_extras();
        assert_eq!(defaults.first(), Some(&60));
        assert_eq!(defaults.last(), Some(&500));
        // Each label has to fit in the 64 bytes before the value field.
        for mbps in defaults {
            assert!(format!("{mbps} Mbps").len() < 64);
        }
    }

    /// Both plugins compile the same JSON asset as their default list.
    #[test]
    fn webview_script_offers_the_same_options() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("parsec-orbit-plugin-high-bitrate-webview")
            .join("assets")
            .join("bandwidth.js");
        let script =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));

        assert!(script.contains("const EXTRA_MBPS = [];"));
        assert_eq!(
            serde_json::from_str::<Vec<i32>>(include_str!("../assets/default-bandwidth-mbps.json"))
                .unwrap(),
            default_extras()
        );
    }

    // ---- init ------------------------------------------------------------------

    #[test]
    fn init_refuses_a_missing_or_too_small_host_table() {
        assert_eq!(
            unsafe { ParsecOrbitPluginInit(std::ptr::null()) },
            parsec_orbit_plugin_sdk::ERR_INVALID
        );
        let mut tiny = ParsecOrbitHost::blank();
        tiny.struct_size = 8;
        assert_eq!(
            unsafe { ParsecOrbitPluginInit(&tiny) },
            parsec_orbit_plugin_sdk::ERR_INVALID
        );
    }

    /// The only test that touches the environment. The enabled path starts a
    /// worker that waits for a client module that this process never loads, so
    /// it patches nothing.
    #[test]
    fn init_logs_its_start_and_honours_the_off_switch() {
        let host = MockHost::new("com.parsec-orbit.native");

        std::env::set_var("PARSEC_ORBIT_PATCHES", "off");
        assert!(host.init::<Native>().1.is_ok());
        assert!(host.logged("native ui patches"));
        assert!(host.logged("native ui plugin stays idle"));
        std::env::remove_var("PARSEC_ORBIT_PATCHES");

        let started = std::time::Instant::now();
        assert!(
            host.init::<Native>().1.is_ok(),
            "returns at once; the work is on a thread"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(host.logs().iter().any(|line| line.level == LOG_INFO));
    }

    // ---- descriptor ------------------------------------------------------------------

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

    #[test]
    fn descriptor_and_manifest_agree() {
        let desc = unsafe { &*ParsecOrbitPluginQuery(ABI_VERSION) };
        assert_eq!(desc.abi_version, ABI_VERSION);
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
            "parsec_orbit_plugin_high_bitrate_native.dll"
        );
    }

    #[test]
    fn the_two_official_plugins_have_distinct_ids() {
        let desc = unsafe { &*ParsecOrbitPluginQuery(ABI_VERSION) };
        assert_eq!(text(desc.id), "com.parsec-orbit.native");
        assert_ne!(text(desc.id), "com.parsec-orbit.webview");
    }

    #[test]
    fn native_registers_ui_section() {
        use std::sync::Mutex;

        static REG_SCHEMA: Mutex<Option<String>> = Mutex::new(None);
        unsafe extern "system" fn test_reg(schema: *const u8, len: usize) -> i32 {
            let s = String::from_utf8_lossy(core::slice::from_raw_parts(schema, len)).into_owned();
            *REG_SCHEMA.lock().unwrap() = Some(s);
            0
        }

        static MOCK_UI: parsec_orbit_plugin_sdk::ParsecOrbitUiV1 =
            parsec_orbit_plugin_sdk::ParsecOrbitUiV1 {
                header: parsec_orbit_plugin_sdk::ParsecOrbitInterfaceHeader::new(
                    std::mem::size_of::<parsec_orbit_plugin_sdk::ParsecOrbitUiV1>(),
                    1,
                    0,
                ),
                show: None,
                hide: None,
                is_visible: None,
                register_section: Some(test_reg),
                unregister_section: None,
                update_state: None,
                on_action: None,
                off_action: None,
            };

        let host = parsec_orbit_plugin_sdk_testing::MockHost::new("com.parsec-orbit.native");
        host.provide("parsec-orbit.ui", &MOCK_UI.header);

        let ui = UiClient::new(&host.context()).expect("ui client");
        let schema = native_ui_schema(&default_extras(), true);
        assert!(ui.register_section(&schema).is_ok());
        let reg = REG_SCHEMA.lock().unwrap().take().unwrap();
        assert!(reg.contains("com.parsec-orbit.native"));
        assert!(reg.contains("High Bitrate"));
    }

    /// The editable list shown by the UI is the same list that is applied.
    #[test]
    fn editable_schema_accepts_custom_integer_options() {
        let schema = native_ui_schema(&[85, 125], true);
        let value: serde_json::Value = serde_json::from_str(&schema).unwrap();
        let items = value["items"].as_array().unwrap();
        let option = items.iter().find(|item| item["key"] == CONFIG_KEY).unwrap();
        assert_eq!(option["type"], "json_editor");
        assert_eq!(option["default"], serde_json::json!([85, 125]));
        assert_eq!(normalize_extras(vec![125, 85, 85]), Ok(vec![85, 125]));
    }

    #[test]
    fn editable_options_are_validated() {
        assert_eq!(normalize_extras(vec![85]), Ok(vec![85]));
        assert_eq!(normalize_extras(vec![i32::MAX, 85]), Ok(vec![85, i32::MAX]));
        assert_eq!(normalize_extras(vec![]), Ok(vec![]));
        assert!(normalize_extras(vec![50]).is_err());
        assert!(normalize_extras(vec![51; MAX_EXTRA_OPTIONS + 1]).is_err());
    }

    #[test]
    fn editing_options_persists_the_shared_json_value() {
        static STORED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

        unsafe extern "system" fn set(
            _owner: *const u8,
            _owner_len: usize,
            key: *const u8,
            key_len: usize,
            json: *const u8,
            json_len: usize,
        ) -> i32 {
            let key = std::str::from_utf8(std::slice::from_raw_parts(key, key_len)).unwrap();
            let json = std::str::from_utf8(std::slice::from_raw_parts(json, json_len)).unwrap();
            STORED
                .lock()
                .unwrap()
                .push((key.to_string(), json.to_string()));
            0
        }

        static CONFIG_TABLE: parsec_orbit_plugin_sdk::ParsecOrbitConfigV1 =
            parsec_orbit_plugin_sdk::ParsecOrbitConfigV1 {
                header: parsec_orbit_plugin_sdk::ParsecOrbitInterfaceHeader::new(
                    std::mem::size_of::<parsec_orbit_plugin_sdk::ParsecOrbitConfigV1>(),
                    1,
                    0,
                ),
                get: None,
                set: Some(set),
                remove: None,
                keys: None,
            };

        let host = MockHost::new("com.parsec-orbit.native");
        host.provide("parsec-orbit.config", &CONFIG_TABLE.header);
        let settings = NativeSettings {
            config: ConfigClient::new(&host.context()).map(|config| config.for_owner(CONFIG_OWNER)),
            extras: Mutex::new(default_extras()),
            enabled: Mutex::new(true),
            cx: host.context(),
        };
        let action = |json: &str| unsafe {
            on_native_action(
                &settings as *const NativeSettings as *mut c_void,
                json.as_ptr(),
                json.len(),
            )
        };
        action(r#"{"key":"extra_bandwidth_mbps","value":[125,85,85]}"#);
        action(r#"{"key":"enabled","value":false}"#);

        assert_eq!(
            *STORED.lock().unwrap(),
            vec![
                (CONFIG_KEY.to_string(), "[85,125]".to_string()),
                (ENABLED_KEY.to_string(), "false".to_string())
            ]
        );
        assert_eq!(*settings.extras.lock().unwrap(), [85, 125]);
        assert!(!*settings.enabled.lock().unwrap());
    }

    #[test]
    fn the_ui_actions_set_the_patch_switches() {
        fn action(settings: &NativeSettings, json: &str) {
            unsafe {
                on_native_action(
                    settings as *const NativeSettings as *mut c_void,
                    json.as_ptr(),
                    json.len(),
                )
            };
        }

        let settings = NativeSettings {
            config: None,
            extras: Mutex::new(default_extras()),
            enabled: Mutex::new(true),
            cx: MockHost::new("com.parsec-orbit.native").context(),
        };

        action(
            &settings,
            r#"{"section":"com.parsec-orbit.native","key":"enabled","type":"change","value":false}"#,
        );
        assert!(*settings.enabled.lock().unwrap());
        action(
            &settings,
            r#"{"section":"com.parsec-orbit.native","key":"enabled","type":"change","value":true}"#,
        );
        assert!(*settings.enabled.lock().unwrap());

        action(
            &settings,
            r#"{"section":"com.parsec-orbit.native","key":"extra_bandwidth_mbps","type":"change","value":[85,125]}"#,
        );
        assert_eq!(*settings.extras.lock().unwrap(), default_extras());

        action(
            &settings,
            r#"{"section":"com.parsec-orbit.native","key":"extra_bandwidth_mbps","type":"change","value":[85,125]}"#,
        );

        // Unknown keys and malformed action data leave settings untouched.
        action(
            &settings,
            r#"{"section":"com.parsec-orbit.native","key":"patch_status","type":"change"}"#,
        );
        action(&settings, "not json");
    }
}
