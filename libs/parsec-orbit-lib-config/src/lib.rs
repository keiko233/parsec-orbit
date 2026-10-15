//! Config store - official Parsec Orbit plugin.
//!
//! Provides `parsec-orbit.config`: settings that survive restarts. Every
//! owner (usually a plugin id) has one JSON object; its keys hold JSON values.
//! The objects live in files below this plugin's data directory
//! (`%APPDATA%\ParsecOrbit\data\com.parsec-orbit.config\<owner>.json`), are
//! cached in memory and written back on every change (a temporary file, then a
//! rename, so a crash never leaves half a file).

pub mod store;

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::OnceLock;

use parsec_orbit_plugin_sdk::prelude::*;
use parsec_orbit_plugin_sdk::services::Config;
use parsec_orbit_plugin_sdk::{
    MessageCallback, ParsecOrbitConfigV1, ParsecOrbitInterfaceHeader, ERR_INVALID, ERR_NOT_FOUND,
    OK,
};

use store::Store;

static STORE: OnceLock<Store> = OnceLock::new();

fn store() -> &'static Store {
    STORE.get_or_init(|| Store::new(None))
}

unsafe fn utf8<'a>(pointer: *const u8, len: usize) -> Option<&'a str> {
    if pointer.is_null() {
        return if len == 0 { Some("") } else { None };
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(pointer, len) }).ok()
}

fn pass(out: Option<MessageCallback>, user: *mut c_void, text: &str) {
    if let Some(out) = out {
        unsafe { out(user, text.as_ptr(), text.len()) };
    }
}

unsafe extern "system" fn get(
    owner: *const u8,
    owner_len: usize,
    key: *const u8,
    key_len: usize,
    out: Option<MessageCallback>,
    user: *mut c_void,
) -> i32 {
    let (Some(owner), Some(key)) = (unsafe { utf8(owner, owner_len) }, unsafe {
        utf8(key, key_len)
    }) else {
        return ERR_INVALID;
    };
    match store().get(owner, key) {
        Ok(Some(json)) => {
            pass(out, user, &json);
            OK
        }
        Ok(None) => ERR_NOT_FOUND,
        Err(code) => code,
    }
}

unsafe extern "system" fn set(
    owner: *const u8,
    owner_len: usize,
    key: *const u8,
    key_len: usize,
    json: *const u8,
    json_len: usize,
) -> i32 {
    let (Some(owner), Some(key), Some(json)) = (
        unsafe { utf8(owner, owner_len) },
        unsafe { utf8(key, key_len) },
        unsafe { utf8(json, json_len) },
    ) else {
        return ERR_INVALID;
    };
    store().set(owner, key, json).err().unwrap_or(OK)
}

unsafe extern "system" fn remove(
    owner: *const u8,
    owner_len: usize,
    key: *const u8,
    key_len: usize,
) -> i32 {
    let (Some(owner), Some(key)) = (unsafe { utf8(owner, owner_len) }, unsafe {
        utf8(key, key_len)
    }) else {
        return ERR_INVALID;
    };
    match store().remove(owner, key) {
        Ok(true) => OK,
        Ok(false) => ERR_NOT_FOUND,
        Err(code) => code,
    }
}

unsafe extern "system" fn keys(
    owner: *const u8,
    owner_len: usize,
    out: Option<MessageCallback>,
    user: *mut c_void,
) -> i32 {
    let Some(owner) = (unsafe { utf8(owner, owner_len) }) else {
        return ERR_INVALID;
    };
    match store().keys(owner) {
        Ok(keys) => {
            pass(out, user, &serde_json::Value::from(keys).to_string());
            OK
        }
        Err(code) => code,
    }
}

/// The table registered under `parsec-orbit.config`.
pub static CONFIG: ParsecOrbitConfigV1 = ParsecOrbitConfigV1 {
    header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitConfigV1>(), 1, 0),
    get: Some(get),
    set: Some(set),
    remove: Some(remove),
    keys: Some(keys),
};

#[derive(Default)]
struct ConfigStore;

impl Plugin for ConfigStore {
    fn init(&mut self, cx: &Context) -> Result {
        info!(cx, "config store {} starting", env!("CARGO_PKG_VERSION"));
        match cx.data_dir() {
            Some(dir) => {
                let _ = STORE.set(Store::new(Some(PathBuf::from(dir))));
            }
            None => warn!(
                cx,
                "no data directory (%APPDATA% is not set): settings are kept in memory only"
            ),
        }
        cx.register::<Config>(&CONFIG)?;
        Ok(())
    }
}

parsec_orbit_plugin_sdk::declare_plugin! {
    id = "com.parsec-orbit.config",
    name = "Config Store",
    version = "0.1.1",
    author = "Parsec Orbit contributors",
    description = "Provides parsec-orbit.config: settings that survive restarts",
    license = "MIT",
    plugin = ConfigStore,
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_plugin_sdk::services::ConfigClient;
    use parsec_orbit_plugin_sdk_testing::MockHost;

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
        let desc = unsafe { &*ParsecOrbitPluginQuery(parsec_orbit_plugin_sdk::ABI_VERSION) };
        assert_eq!(text(desc.id), manifest_value("id"));
        assert_eq!(text(desc.name), manifest_value("name"));
        assert_eq!(text(desc.version), manifest_value("version"));
        assert_eq!(text(desc.description), manifest_value("description"));
        assert_eq!(text(desc.license), manifest_value("license"));
        assert_eq!(text(desc.author), manifest_value("author"));
        assert_eq!(manifest_value("entry"), "parsec_orbit_lib_config.dll");
    }

    /// The whole path a consumer takes: the service registered by `init`, the
    /// SDK client on top, the store behind.
    #[test]
    fn a_client_reads_and_writes_through_the_registered_service() {
        let dir =
            std::env::temp_dir().join(format!("parsec-orbit-config-plugin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let host = MockHost::new("com.parsec-orbit.config").with_data_dir(&dir);
        host.init::<ConfigStore>().1.unwrap();
        assert!(host.registered("parsec-orbit.config"));

        // The mock registry is per thread and shared by every mock plugin,
        // so a second mock plugin sees the service the first one registered.
        let consumer = MockHost::new("com.example.consumer");
        let client = ConfigClient::new(&consumer.context()).expect("the service");
        assert!(!client.get("answer", |_| {}).unwrap());
        client.set("answer", "{\"n\":42}").unwrap();
        let mut seen = String::new();
        assert!(client
            .get("answer", |json| seen = json.to_string())
            .unwrap());
        assert_eq!(seen, "{\"n\":42}");

        let mut keys = String::new();
        client.keys(|json| keys = json.to_string()).unwrap();
        assert_eq!(keys, "[\"answer\"]");
        assert!(client.remove("answer").unwrap());
        assert!(!client.remove("answer").unwrap());
        assert_eq!(
            client.set("bad", "not json").unwrap_err().code(),
            ERR_INVALID
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
