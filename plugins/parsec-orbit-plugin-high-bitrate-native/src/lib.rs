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
use parsec_orbit_plugin_sdk::services::ui::UiClient;
use parsec_orbit_plugin_sdk::services::ConfigClient;

const CONFIG_OWNER: &str = "com.parsec-orbit.native";
const PAGE_ID: &str = "com.parsec-orbit.native";
const STATUS_KEY: &str = "patch_status";
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

/// The additional options as the page shows them: `85, 125`.
fn format_extras(extras: &[i32]) -> String {
    extras
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Reads what the user typed: integers separated by commas or spaces.
fn parse_extras(text: &str) -> std::result::Result<Vec<i32>, String> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|token| !token.is_empty())
        .map(|token| {
            token
                .parse::<i32>()
                .map_err(|_| format!("\"{token}\" is not an integer"))
        })
        .collect()
}

fn native_ui_page(extras: &[i32], enabled: bool) -> String {
    serde_json::json!({
        "schema": 2,
        "id": PAGE_ID,
        "title": "High Bitrate",
        "icon": "speed",
        "order": 10,
        "description": "Bandwidth limit options for the native (libmatoya) client UI.",
        "blocks": [
            {"type": "group", "title": "Bandwidth ladder", "settings": [
                {
                    "key": ENABLED_KEY,
                    "label": "Extend the bandwidth ladder",
                    "description": "Append the options above 50 Mbps to Parsec's Bandwidth Limit dropdown.",
                    "control": {"type": "toggle", "default": enabled}
                },
                {
                    "key": CONFIG_KEY,
                    "label": "Additional options",
                    "description": "Mbps values above 50, separated by commas. Values are sorted and duplicates removed; up to 128.",
                    "control": {
                        "type": "text",
                        "placeholder": "85, 125, 200",
                        "default": format_extras(extras)
                    }
                }
            ]},
            {"type": "group", "title": "Status", "settings": [
                {
                    "key": STATUS_KEY,
                    "label": "Patch status",
                    "description": "What the in-memory patch did.",
                    "control": {"type": "value", "default": "Waiting for the client UI..."}
                }
            ]}
        ]
    })
    .to_string()
}

fn update_page(cx: &Context, patch: serde_json::Value) {
    if let Some(ui) = UiClient::new(cx) {
        let _ = ui.update_page(PAGE_ID, &patch.to_string());
    }
}

/// Logs a line and shows it as the patch status.
fn report(cx: &Context, message: &str) {
    info!(cx, "{message}");
    update_page(cx, serde_json::json!({"values": {STATUS_KEY: message}}));
}

/// Shows the outcome of an edit beside the setting. A saved change applies
/// only after the client restarts, which the page announces.
fn outcome(cx: &Context, key: &str, saved: bool, message: &str) {
    info!(cx, "{message}");
    let severity = if saved { "success" } else { "error" };
    let mut patch = serde_json::json!({
        "props": {key: {"status": {"severity": severity, "text": message}}}
    });
    if saved {
        patch["restart_required"] = serde_json::json!(true);
    }
    update_page(cx, patch);
}

/// Handles the page's controls. The state remains alive for the plugin's
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
        Some(ENABLED_KEY) => {
            let cx = &settings.cx;
            let Some(enabled) = value.and_then(|value| value.as_bool()) else {
                outcome(cx, ENABLED_KEY, false, "Not saved: expected on or off.");
                return;
            };
            let Some(config) = settings.config else {
                outcome(
                    cx,
                    ENABLED_KEY,
                    false,
                    "Not saved: the config service is unavailable.",
                );
                return;
            };
            match config.set(ENABLED_KEY, if enabled { "true" } else { "false" }) {
                Ok(()) => {
                    *settings.enabled.lock().unwrap() = enabled;
                    outcome(
                        cx,
                        ENABLED_KEY,
                        true,
                        "Saved. Restart the client to apply it.",
                    );
                }
                Err(error) => outcome(
                    cx,
                    ENABLED_KEY,
                    false,
                    &format!("Not saved (config error {}).", error.code()),
                ),
            }
        }
        Some(CONFIG_KEY) => {
            let cx = &settings.cx;
            let Some(text) = value.and_then(|value| value.as_str()) else {
                outcome(
                    cx,
                    CONFIG_KEY,
                    false,
                    "Not saved: expected a list of numbers.",
                );
                return;
            };
            let extras = match parse_extras(text)
                .and_then(|parsed| normalize_extras(parsed).map_err(str::to_string))
            {
                Ok(extras) => extras,
                Err(error) => {
                    outcome(cx, CONFIG_KEY, false, &format!("Not saved: {error}."));
                    return;
                }
            };
            let Ok(json) = serde_json::to_string(&extras) else {
                outcome(
                    cx,
                    CONFIG_KEY,
                    false,
                    "Not saved: could not encode the integer list.",
                );
                return;
            };
            let Some(config) = settings.config else {
                outcome(
                    cx,
                    CONFIG_KEY,
                    false,
                    "Not saved: the config service is unavailable.",
                );
                return;
            };
            match config.set(CONFIG_KEY, &json) {
                Ok(()) => {
                    // The page shows the normalized list: sorted, without duplicates.
                    update_page(
                        cx,
                        serde_json::json!({"values": {CONFIG_KEY: format_extras(&extras)}}),
                    );
                    *settings.extras.lock().unwrap() = extras;
                    outcome(
                        cx,
                        CONFIG_KEY,
                        true,
                        "Saved. Restart the client to apply the options.",
                    );
                }
                Err(error) => outcome(
                    cx,
                    CONFIG_KEY,
                    false,
                    &format!("Not saved (config error {}).", error.code()),
                ),
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
            let page = native_ui_page(&extras, persisted_enabled);
            let _ = ui.register_page(&page);
            // Plugins are not unloaded during this process; keep the callback
            // state alive for the UI service's subscription lifetime.
            let callback_state = Arc::into_raw(settings.clone()) as *mut c_void;
            let _ = unsafe { ui.on_action(PAGE_ID, on_native_action, callback_state) };
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
    fn native_registers_ui_page() {
        use std::sync::Mutex;

        static REG_PAGE: Mutex<Option<String>> = Mutex::new(None);
        unsafe extern "system" fn test_reg(page: *const u8, len: usize) -> i32 {
            let s = String::from_utf8_lossy(core::slice::from_raw_parts(page, len)).into_owned();
            *REG_PAGE.lock().unwrap() = Some(s);
            0
        }

        static MOCK_UI: parsec_orbit_plugin_sdk::ParsecOrbitUiV2 =
            parsec_orbit_plugin_sdk::ParsecOrbitUiV2 {
                header: parsec_orbit_plugin_sdk::ParsecOrbitInterfaceHeader::new(
                    std::mem::size_of::<parsec_orbit_plugin_sdk::ParsecOrbitUiV2>(),
                    2,
                    0,
                ),
                show: None,
                hide: None,
                is_visible: None,
                navigate: None,
                register_page: Some(test_reg),
                unregister_page: None,
                update_page: None,
                on_action: None,
                off_action: None,
            };

        let host = parsec_orbit_plugin_sdk_testing::MockHost::new("com.parsec-orbit.native");
        host.provide("parsec-orbit.ui", &MOCK_UI.header);

        let ui = UiClient::new(&host.context()).expect("ui client");
        let page = native_ui_page(&default_extras(), true);
        assert!(ui.register_page(&page).is_ok());
        let reg = REG_PAGE.lock().unwrap().take().unwrap();
        assert!(reg.contains("com.parsec-orbit.native"));
        assert!(reg.contains("High Bitrate"));
    }

    /// The page must pass the service's own validation, or registration is refused.
    #[test]
    fn the_page_satisfies_the_service_schema() {
        for (extras, enabled) in [
            (default_extras(), true),
            (vec![85, 125], false),
            (vec![], true),
        ] {
            let page = native_ui_page(&extras, enabled);
            let parsed = parsec_orbit_ui_protocol::UiPage::parse(&page, false)
                .unwrap_or_else(|error| panic!("{error}: {page}"));
            let values = parsed.initial_values();
            assert_eq!(values[ENABLED_KEY], serde_json::json!(enabled));
            assert_eq!(
                values[CONFIG_KEY],
                serde_json::json!(format_extras(&extras))
            );
        }
    }

    #[test]
    fn extra_options_are_read_and_shown_as_a_comma_separated_list() {
        assert_eq!(parse_extras("85, 125"), Ok(vec![85, 125]));
        assert_eq!(parse_extras("85 125,200"), Ok(vec![85, 125, 200]));
        assert_eq!(parse_extras(" "), Ok(vec![]));
        assert!(parse_extras("85, fast").unwrap_err().contains("\"fast\""));
        assert!(parse_extras("85.5").is_err());
        assert_eq!(format_extras(&[85, 125]), "85, 125");
        assert_eq!(format_extras(&[]), "");
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

    /// A provider that records every patch it is sent. It is shared, so the
    /// tests that use it hold `SERIAL` for their whole run.
    static PATCHES: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static SERIAL: Mutex<()> = Mutex::new(());

    unsafe extern "system" fn record_patch(
        _page: *const u8,
        _page_len: usize,
        patch: *const u8,
        patch_len: usize,
    ) -> i32 {
        let patch = std::str::from_utf8(std::slice::from_raw_parts(patch, patch_len)).unwrap();
        PATCHES.lock().unwrap().push(patch.to_string());
        0
    }

    static RECORDING_UI: parsec_orbit_plugin_sdk::ParsecOrbitUiV2 =
        parsec_orbit_plugin_sdk::ParsecOrbitUiV2 {
            header: parsec_orbit_plugin_sdk::ParsecOrbitInterfaceHeader::new(
                std::mem::size_of::<parsec_orbit_plugin_sdk::ParsecOrbitUiV2>(),
                2,
                0,
            ),
            show: None,
            hide: None,
            is_visible: None,
            navigate: None,
            register_page: None,
            unregister_page: None,
            update_page: Some(record_patch),
            on_action: None,
            off_action: None,
        };

    #[test]
    fn editing_options_persists_the_shared_json_value() {
        let _serial = SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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

        PATCHES.lock().unwrap().clear();
        let host = MockHost::new("com.parsec-orbit.native");
        host.provide("parsec-orbit.config", &CONFIG_TABLE.header);
        host.provide("parsec-orbit.ui", &RECORDING_UI.header);
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
        action(r#"{"key":"extra_bandwidth_mbps","value":"125, 85, 85"}"#);
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

        // The page shows the normalized list and announces that a restart is needed.
        let patches = PATCHES.lock().unwrap().clone();
        let patches: Vec<serde_json::Value> = patches
            .iter()
            .map(|patch| serde_json::from_str(patch).unwrap())
            .collect();
        assert!(patches
            .iter()
            .any(|patch| patch["values"][CONFIG_KEY] == "85, 125"));
        let saved: Vec<_> = patches
            .iter()
            .filter(|patch| patch["restart_required"] == true)
            .collect();
        assert_eq!(saved.len(), 2);
        assert_eq!(
            saved[0]["props"][CONFIG_KEY]["status"]["severity"],
            serde_json::json!("success")
        );
    }

    #[test]
    fn invalid_edits_show_an_error_and_change_nothing() {
        let _serial = SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        PATCHES.lock().unwrap().clear();
        let host = MockHost::new("com.parsec-orbit.native");
        host.provide("parsec-orbit.ui", &RECORDING_UI.header);
        let settings = NativeSettings {
            config: None,
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

        // Without the config service nothing can be persisted.
        action(
            r#"{"page":"com.parsec-orbit.native","key":"enabled","type":"change","value":false}"#,
        );
        assert!(*settings.enabled.lock().unwrap());
        action(r#"{"key":"extra_bandwidth_mbps","type":"change","value":"85, 125"}"#);
        assert_eq!(*settings.extras.lock().unwrap(), default_extras());
        // Bad input is reported against the setting.
        action(r#"{"key":"extra_bandwidth_mbps","type":"change","value":"40"}"#);
        action(r#"{"key":"extra_bandwidth_mbps","type":"change","value":"fast"}"#);
        // Unknown keys and malformed action data leave settings untouched.
        action(r#"{"key":"patch_status","type":"change"}"#);
        action("not json");
        assert_eq!(*settings.extras.lock().unwrap(), default_extras());

        let patches = PATCHES.lock().unwrap().clone();
        assert!(!patches.is_empty());
        assert!(patches
            .iter()
            .any(|patch| patch.contains(r#""severity":"error""#)));
        assert!(patches
            .iter()
            .all(|patch| !patch.contains("restart_required")));
    }
}
