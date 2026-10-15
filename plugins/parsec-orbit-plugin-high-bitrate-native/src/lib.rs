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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use parsec_orbit_plugin_sdk::prelude::*;
use parsec_orbit_plugin_sdk::services::UiClient;

/// Bandwidth options appended after the built-in `3..50 mbps` ladder.
///
/// Keep in sync with the WebView plugin's `assets/bandwidth.js`.
pub const EXTRA_BANDWIDTH_MBPS: [i32; 10] = [60, 75, 100, 125, 150, 200, 250, 300, 400, 500];

/// The values the Orbit UI offers as "Highest option".
pub const MAX_PRESETS: [i32; 4] = [100, 200, 300, 500];

/// Whether the patch runs at all, as chosen in the Orbit UI. The environment
/// switch [`enabled`] still wins: it is the older, documented way to keep the
/// plugin idle.
static PATCH_ENABLED: AtomicBool = AtomicBool::new(true);

/// Highest bandwidth the dropdown should offer. Read when the patch runs.
static MAX_PRESET: AtomicUsize = AtomicUsize::new(500);

/// `PARSEC_ORBIT_PATCHES=0` (or `off`/`false`) keeps the plugin idle. The switch
/// predates the plugin split, when the patch was compiled into the loader, and
/// is kept so existing setups keep working.
fn enabled() -> bool {
    enabled_with(std::env::var("PARSEC_ORBIT_PATCHES").ok().as_deref())
}

fn enabled_with(switch: Option<&str>) -> bool {
    !matches!(switch, Some("0") | Some("off") | Some("false"))
}

/// The Orbit UI section for this plugin. Both settings are read when the patch
/// runs, which is while the client maps its UI module, so the descriptions say
/// what a change actually costs.
const NATIVE_UI_SCHEMA: &str = r#"{
    "id": "com.parsec-orbit.native",
    "title": "High Bitrate",
    "description": "Bandwidth limit options for the native (libmatoya) client UI",
    "order": 10,
    "items": [
        {
            "key": "enabled",
            "type": "toggle",
            "label": "Extend the bandwidth ladder",
            "description": "Append the options above 50 Mbps to Parsec's Bandwidth Limit dropdown",
            "default": true
        },
        {
            "key": "max_preset",
            "type": "select",
            "label": "Highest option",
            "description": "Highest bandwidth the dropdown offers",
            "default": "500",
            "options": [
                {"label": "100 Mbps", "value": "100"},
                {"label": "200 Mbps", "value": "200"},
                {"label": "300 Mbps", "value": "300"},
                {"label": "500 Mbps", "value": "500"}
            ]
        },
        {
            "key": "patch_status",
            "type": "readonly_text",
            "label": "Patch status",
            "description": "What the in-memory patch did, written by the plugin",
            "default": "Waiting for the client UI..."
        }
    ]
}"#;

/// The options to append, cut off at the highest bandwidth the user chose.
fn selected_extras() -> Vec<i32> {
    let highest = MAX_PRESET.load(Ordering::Acquire) as i32;
    EXTRA_BANDWIDTH_MBPS
        .iter()
        .copied()
        .filter(|mbps| *mbps <= highest)
        .collect()
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

/// Handles the section's controls: `enabled` and `max_preset` are remembered
/// for the patch, and the status row is written by [`report`].
unsafe extern "system" fn on_native_action(_user: *mut c_void, msg: *const u8, len: usize) {
    let slice = unsafe { std::slice::from_raw_parts(msg, len) };
    let Ok(action) = serde_json::from_slice::<serde_json::Value>(slice) else {
        return;
    };
    let value = action.get("value");
    match action.get("key").and_then(|key| key.as_str()) {
        Some("enabled") => {
            if let Some(enabled) = value.and_then(|value| value.as_bool()) {
                PATCH_ENABLED.store(enabled, Ordering::Release);
            }
        }
        Some("max_preset") => {
            let chosen = value
                .and_then(|value| value.as_str())
                .and_then(|text| text.parse::<i32>().ok())
                .or_else(|| value.and_then(|value| value.as_i64()).map(|n| n as i32));
            if let Some(chosen) = chosen.filter(|chosen| MAX_PRESETS.contains(chosen)) {
                MAX_PRESET.store(chosen as usize, Ordering::Release);
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
        if let Some(ui) = UiClient::new(cx) {
            let _ = ui.register_section(NATIVE_UI_SCHEMA);
            let _ = unsafe {
                ui.on_action(
                    "com.parsec-orbit.native",
                    on_native_action,
                    std::ptr::null_mut(),
                )
            };
        }

        let cx = *cx;
        std::thread::Builder::new()
            .name(format!("{}:patch", cx.plugin_id()))
            .spawn(move || {
                if !PATCH_ENABLED.load(Ordering::Acquire) {
                    report(
                        &cx,
                        "switched off in the Orbit UI, the bandwidth ladder is unchanged",
                    );
                    return;
                }
                legacy::extend_bandwidth_with(selected_extras, |message| report(&cx, message));
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
        assert!(
            EXTRA_BANDWIDTH_MBPS.iter().all(|mbps| *mbps > 50),
            "the UI already offers up to 50"
        );
        assert!(
            EXTRA_BANDWIDTH_MBPS
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
            "ascending, no duplicates"
        );
        assert_eq!(EXTRA_BANDWIDTH_MBPS.first(), Some(&60));
        assert_eq!(EXTRA_BANDWIDTH_MBPS.last(), Some(&500));
        // Each label has to fit in the 64 bytes before the value field.
        for mbps in EXTRA_BANDWIDTH_MBPS {
            assert!(format!("{mbps} Mbps").len() < 64);
        }
    }

    /// `assets/bandwidth.js` of the WebView plugin carries its own copy of the
    /// list ("keep in sync"); the two UIs must offer the same options.
    #[test]
    fn webview_script_offers_the_same_options() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("parsec-orbit-plugin-high-bitrate-webview")
            .join("assets")
            .join("bandwidth.js");
        let script =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));

        let start = script
            .find("const EXTRA_MBPS = [")
            .expect("EXTRA_MBPS in bandwidth.js")
            + "const EXTRA_MBPS = [".len();
        let end = start + script[start..].find(']').unwrap();
        let in_script: Vec<i32> = script[start..end]
            .split(',')
            .map(|number| number.trim().parse().expect("a number"))
            .collect();

        assert_eq!(in_script, EXTRA_BANDWIDTH_MBPS);
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
        assert!(ui.register_section(NATIVE_UI_SCHEMA).is_ok());
        let reg = REG_SCHEMA.lock().unwrap().take().unwrap();
        assert!(reg.contains("com.parsec-orbit.native"));
        assert!(reg.contains("High Bitrate"));
    }

    /// Both settings are read when the patch runs, so the schema has to offer
    /// exactly the presets the code accepts and no control without a setting
    /// behind it.
    #[test]
    fn the_schema_offers_the_presets_the_code_accepts() {
        for preset in MAX_PRESETS {
            assert!(
                NATIVE_UI_SCHEMA.contains(&format!("\"value\": \"{preset}\"")),
                "the schema offers {preset} Mbps"
            );
        }
        assert!(NATIVE_UI_SCHEMA.contains("\"key\": \"patch_status\""));
        assert!(
            !NATIVE_UI_SCHEMA.contains("custom_bitrate"),
            "a target bitrate nothing applies is worse than no control"
        );
    }

    #[test]
    fn the_highest_option_cuts_the_extra_ladder() {
        let previous = MAX_PRESET.swap(100, Ordering::AcqRel);
        assert_eq!(selected_extras(), vec![60, 75, 100]);
        assert_eq!(selected_extras().last(), Some(&100));

        MAX_PRESET.store(500, Ordering::Release);
        assert_eq!(selected_extras(), EXTRA_BANDWIDTH_MBPS.to_vec());

        MAX_PRESET.store(previous, Ordering::Release);
    }

    #[test]
    fn the_ui_actions_set_the_patch_switches() {
        fn action(json: &str) {
            unsafe { on_native_action(std::ptr::null_mut(), json.as_ptr(), json.len()) };
        }

        action(
            r#"{"section":"com.parsec-orbit.native","key":"enabled","type":"change","value":false}"#,
        );
        assert!(!PATCH_ENABLED.load(Ordering::Acquire));
        action(
            r#"{"section":"com.parsec-orbit.native","key":"enabled","type":"change","value":true}"#,
        );
        assert!(PATCH_ENABLED.load(Ordering::Acquire));

        action(
            r#"{"section":"com.parsec-orbit.native","key":"max_preset","type":"change","value":"200"}"#,
        );
        assert_eq!(MAX_PRESET.load(Ordering::Acquire), 200);
        assert_eq!(selected_extras().last(), Some(&200));

        // A value outside the offered presets leaves the choice alone.
        action(
            r#"{"section":"com.parsec-orbit.native","key":"max_preset","type":"change","value":"999"}"#,
        );
        assert_eq!(MAX_PRESET.load(Ordering::Acquire), 200);

        // Unknown keys and broken payloads are ignored rather than panicking.
        action(r#"{"section":"com.parsec-orbit.native","key":"patch_status","type":"change"}"#);
        action("not json");

        MAX_PRESET.store(500, Ordering::Release);
        PATCH_ENABLED.store(true, Ordering::Release);
    }
}
