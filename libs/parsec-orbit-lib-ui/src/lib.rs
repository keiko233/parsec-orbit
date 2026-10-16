//! Orbit UI Service - official Parsec Orbit plugin.
//!
//! Provides `parsec-orbit.ui`: an external companion window powered by `compio-rs/winio`
//! driven by unified declarative schemas across Rust and JavaScript.

mod builtins;
pub mod companion;
#[cfg(windows)]
mod maintenance;
pub mod registry;
pub mod schema;
pub mod service;

#[cfg(windows)]
pub mod hotkey;
#[cfg(windows)]
pub mod instance;

use parsec_orbit_plugin_sdk::prelude::*;
use parsec_orbit_plugin_sdk::services::ui::Ui;

#[derive(Default)]
pub struct OrbitUiPlugin {
    _builtins: Option<builtins::Builtins>,
}

impl Plugin for OrbitUiPlugin {
    fn init(&mut self, cx: &Context) -> Result {
        info!(
            cx,
            "orbit ui service {} starting",
            env!("CARGO_PKG_VERSION")
        );
        cx.register::<Ui>(&service::UI_SERVICE)?;

        #[cfg(windows)]
        {
            // The client runs twice: a window in the service's session is
            // invisible to the user and its hotkeys never reach this desktop,
            // so the instance the user started owns the companion window.
            if !instance::is_client() {
                info!(
                    cx,
                    "orbit ui: the host instance keeps the service only; the companion window belongs to the client instance"
                );
                return Ok(());
            }
            let cx = *cx;
            #[cfg(not(test))]
            if let Err(error) = service::start(cx) {
                warn!(cx, "orbit ui: could not start companion: {}", error);
            }
            #[cfg(not(test))]
            {
                match builtins::start(cx) {
                    Ok(panels) => self._builtins = Some(panels),
                    Err(error) => warn!(cx, "orbit ui: built-in panels unavailable: {}", error),
                }
            }
            std::thread::Builder::new()
                .name(format!("{}:hotkey", cx.plugin_id()))
                .spawn(move || {
                    hotkey::run(service::toggle, move |message| {
                        warn!(cx, "orbit ui: {}", message)
                    })
                })?;
        }

        #[cfg(all(target_os = "macos", not(test)))]
        if let Err(error) = service::start(*cx) {
            warn!(cx, "orbit ui: could not start companion: {}", error);
        }
        #[cfg(all(target_os = "macos", not(test)))]
        {
            match builtins::start(*cx) {
                Ok(panels) => self._builtins = Some(panels),
                Err(error) => warn!(cx, "orbit ui: built-in panels unavailable: {}", error),
            }
        }

        Ok(())
    }
}

parsec_orbit_plugin_sdk::declare_plugin! {
    id = "com.parsec-orbit.ui",
    name = "Orbit UI Service",
    version = "0.1.1",
    author = "Parsec Orbit contributors",
    description = "Provides parsec-orbit.ui: companion window for Orbit and plugin settings",
    license = "MIT",
    plugin = OrbitUiPlugin,
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_plugin_sdk::services::ui::UiClient;
    use parsec_orbit_plugin_sdk::{ERR_EXISTS, ERR_INVALID, ERR_NOT_FOUND};
    use parsec_orbit_plugin_sdk_testing::MockHost;
    use std::ffi::c_void;
    use std::sync::Mutex;

    static ACTION_RECEIVED: Mutex<Option<String>> = Mutex::new(None);

    unsafe extern "system" fn test_action_sink(user: *mut c_void, msg: *const u8, len: usize) {
        let slice = unsafe { core::slice::from_raw_parts(msg, len) };
        *ACTION_RECEIVED.lock().unwrap() =
            Some(format!("{}:{:?}", String::from_utf8_lossy(slice), user));
    }

    #[test]
    fn descriptor_and_manifest_agree() {
        let manifest = include_str!("../plugin.toml");
        let desc = unsafe { &*ParsecOrbitPluginQuery(parsec_orbit_plugin_sdk::ABI_VERSION) };

        let manifest_value = |key: &str| -> String {
            manifest
                .lines()
                .find(|l| l.trim_start().starts_with(key))
                .and_then(|l| l.split('=').nth(1))
                .map(|v| v.trim().trim_matches('"').to_string())
                .unwrap_or_default()
        };

        let text = |ptr: *const u8| -> String {
            if ptr.is_null() {
                return String::new();
            }
            let mut len = 0;
            while unsafe { *ptr.add(len) } != 0 {
                len += 1;
            }
            let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
            std::str::from_utf8(slice).unwrap().to_string()
        };

        assert_eq!(text(desc.id), manifest_value("id"));
        assert_eq!(text(desc.name), manifest_value("name"));
        assert_eq!(text(desc.version), manifest_value("version"));
        assert_eq!(text(desc.license), manifest_value("license"));
        assert_eq!(text(desc.author), manifest_value("author"));
        assert_eq!(manifest_value("entry"), "parsec_orbit_lib_ui.dll");
    }

    #[test]
    fn a_client_calls_through_registered_ui_service() {
        let host = MockHost::new("com.parsec-orbit.ui");
        host.init::<OrbitUiPlugin>().1.unwrap();
        assert!(host.registered("parsec-orbit.ui"));

        let consumer = MockHost::new("com.example.consumer");
        let client = UiClient::new(&consumer.context()).expect("the ui service");

        assert!(client.show().is_ok());
        assert!(client.hide().is_ok());
        assert_eq!(client.is_visible().unwrap(), false);

        // Invalid page
        assert_eq!(
            client.register_page("not json").unwrap_err().code(),
            ERR_INVALID
        );

        // Valid page
        let page = r#"{
            "schema": 2,
            "id": "consumer.tuning",
            "title": "Tuning",
            "blocks": [{"type": "group", "settings": [
                {"key": "fps", "label": "Max FPS",
                 "control": {"type": "number", "integer": true, "min": 1, "max": 240, "default": 60}}
            ]}]
        }"#;
        assert!(client.register_page(page).is_ok());

        // Duplicate registration
        assert_eq!(client.register_page(page).unwrap_err().code(), ERR_EXISTS);

        // Reserved identifiers belong to the service
        assert_eq!(
            client
                .register_page(&page.replace("consumer.tuning", "orbit.fake"))
                .unwrap_err()
                .code(),
            ERR_INVALID
        );

        // Navigation and patches
        assert!(client.navigate("consumer.tuning").is_ok());
        assert_eq!(
            client.navigate("nonexistent").unwrap_err().code(),
            ERR_NOT_FOUND
        );
        assert!(client
            .update_page("consumer.tuning", r#"{"values": {"fps": 120}}"#)
            .is_ok());
        assert_eq!(
            client
                .update_page("consumer.tuning", r#"{"values": {"fps": 1000}}"#)
                .unwrap_err()
                .code(),
            ERR_INVALID
        );
        assert_eq!(
            client
                .update_page("nonexistent", r#"{"values": {"fps": 120}}"#)
                .unwrap_err()
                .code(),
            ERR_NOT_FOUND
        );
        assert_eq!(
            service::registry()
                .get_page("consumer.tuning")
                .unwrap()
                .values["fps"],
            serde_json::Value::from(120)
        );

        // Action subscription
        let sub = unsafe {
            client
                .on_action("consumer.tuning", test_action_sink, 0x42 as *mut c_void)
                .unwrap()
        };
        assert!(sub > 0);

        service::registry().dispatch_action(&schema::UiAction {
            page: "consumer.tuning".to_string(),
            key: "fps".to_string(),
            action_type: "change".to_string(),
            value: Some(serde_json::Value::from(144)),
        });

        let msg = ACTION_RECEIVED
            .lock()
            .unwrap()
            .take()
            .expect("action event dispatched");
        assert!(msg.contains("\"key\":\"fps\""));
        assert!(msg.contains("\"value\":144"));

        // Unsubscribe
        assert!(client.off_action(sub).is_ok());
        assert_eq!(client.off_action(sub).unwrap_err().code(), ERR_NOT_FOUND);

        // Unregister page
        assert!(client.unregister_page("consumer.tuning").is_ok());
        assert_eq!(
            client
                .unregister_page("consumer.tuning")
                .unwrap_err()
                .code(),
            ERR_NOT_FOUND
        );
    }
}
