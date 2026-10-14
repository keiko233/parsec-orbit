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

use std::ffi::c_void;
use parsec_orbit_plugin_sdk_std::prelude::*;
use parsec_orbit_plugin_sdk_std::services::UiClient;

/// Bandwidth options appended after the built-in `3..50 mbps` ladder.
///
/// Keep in sync with the WebView plugin's `assets/bandwidth.js`.
pub const EXTRA_BANDWIDTH_MBPS: [i32; 10] = [60, 75, 100, 125, 150, 200, 250, 300, 400, 500];

/// `PARSEC_ORBIT_PATCHES=0` (or `off`/`false`) keeps the plugin idle. The switch
/// predates the plugin split, when the patch was compiled into the loader, and
/// is kept so existing setups keep working.
fn enabled() -> bool {
    enabled_with(std::env::var("PARSEC_ORBIT_PATCHES").ok().as_deref())
}

fn enabled_with(switch: Option<&str>) -> bool {
    !matches!(switch, Some("0") | Some("off") | Some("false"))
}


const NATIVE_UI_SCHEMA: &str = r#"{
    "id": "com.parsec-orbit.native",
    "title": "High Bitrate (Native)",
    "description": "Bandwidth limit presets and options for Parsec Native UI",
    "order": 10,
    "items": [
        {
            "key": "enabled",
            "type": "toggle",
            "label": "Extend Bandwidth Ladder",
            "description": "Patch in-memory table to append 60..500 Mbps options to settings",
            "default": true
        },
        {
            "key": "max_preset",
            "type": "select",
            "label": "Maximum Bandwidth Option",
            "description": "Highest selectable bandwidth in Parsec settings",
            "default": "500 Mbps",
            "options": [
                {"label": "100 Mbps", "value": "100"},
                {"label": "200 Mbps", "value": "200"},
                {"label": "300 Mbps", "value": "300"},
                {"label": "500 Mbps", "value": "500"}
            ]
        },
        {
            "key": "custom_bitrate",
            "type": "slider",
            "label": "Target Bitrate (Mbps)",
            "description": "Preferred encoder bitrate target",
            "min": 10.0,
            "max": 500.0,
            "step": 10.0,
            "default": 100.0
        },
        {
            "key": "patch_status",
            "type": "text",
            "label": "Patch Status",
            "description": "In-memory UI patch status",
            "default": "Waiting for client UI..."
        }
    ]
}"#;

unsafe extern "system" fn on_native_action(_user: *mut c_void, _msg: *const u8, _len: usize) {}

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
            let _ = unsafe { ui.on_action("com.parsec-orbit.native", on_native_action, std::ptr::null_mut()) };
        }

        let cx = *cx;
        cx.spawn("patch", move || {
            legacy::extend_bandwidth(|message| {
                info!(cx, "{message}");
                if let Some(ui) = UiClient::new(&cx) {
                    let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
                    let json = format!(r#"{{"patch_status":"{escaped}"}}"#);
                    let _ = ui.update_state("com.parsec-orbit.native", &json);
                }
            });
        })?;

        Ok(())
    }
}

parsec_orbit_plugin_sdk_std::declare_plugin! {
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
    use parsec_orbit_plugin_sdk_std::testing::MockHost;
    use parsec_orbit_plugin_sdk_std::{ParsecOrbitHost, ABI_VERSION, LOG_INFO};
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
            parsec_orbit_plugin_sdk_std::ERR_INVALID
        );
        let mut tiny = ParsecOrbitHost::blank();
        tiny.struct_size = 8;
        assert_eq!(
            unsafe { ParsecOrbitPluginInit(&tiny) },
            parsec_orbit_plugin_sdk_std::ERR_INVALID
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

        static MOCK_UI: parsec_orbit_plugin_sdk_std::ParsecOrbitUiV1 = parsec_orbit_plugin_sdk_std::ParsecOrbitUiV1 {
            header: parsec_orbit_plugin_sdk_std::ParsecOrbitInterfaceHeader::new(
                std::mem::size_of::<parsec_orbit_plugin_sdk_std::ParsecOrbitUiV1>(),
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

        let host = parsec_orbit_plugin_sdk_std::testing::MockHost::new("com.parsec-orbit.native");
        host.provide("parsec-orbit.ui", &MOCK_UI.header);

        let ui = UiClient::new(&host.context()).expect("ui client");
        assert!(ui.register_section(NATIVE_UI_SCHEMA).is_ok());
        let reg = REG_SCHEMA.lock().unwrap().take().unwrap();
        assert!(reg.contains("com.parsec-orbit.native"));
        assert!(reg.contains("High Bitrate"));
    }
}
