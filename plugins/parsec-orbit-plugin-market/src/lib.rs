//! Market - official Parsec Orbit plugin that browses, installs, updates and
//! removes plugins from the signed registries, inside the client.
//!
//! * [`controller`] holds the state of the Market section and turns UI actions
//!   into commands.
//! * [`worker`] runs the commands on one thread: registry, disk and the section.
//!
//! The design is `docs/design/restructure.md` section 4; the section keys are
//! in `docs/spec/registry.md`.
//!
//! The UI service runs its callbacks on its own delivery thread and they must
//! not block, so the callback only queues the action. The queue's sender lives
//! in a `static`: the ABI callback has no context pointer, which is also why
//! the native UI plugin keeps its state in statics.

pub mod controller;
pub mod worker;

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

use parsec_orbit_plugin_sdk::prelude::*;
use parsec_orbit_plugin_sdk::services::ui::UiClient;
use parsec_orbit_registry::index::PlatformId;
use parsec_orbit_registry::source::Real;
use parsec_orbit_registry::sources;

use controller::{Action, PAGE_ID};
use worker::{PagePublisher, Worker};

/// The one queue the UI callback feeds. There is one market per process.
static ACTIONS: OnceLock<Sender<Action>> = OnceLock::new();

/// The Orbit root for a data directory `<root>/data/<plugin id>`, or `None` when
/// the directory does not have that shape.
fn root_of_data_dir(data_dir: &Path, plugin_id: &str) -> Option<PathBuf> {
    if data_dir.file_name()?.to_str()? != plugin_id {
        return None;
    }
    let data = data_dir.parent()?;
    if data.file_name()?.to_str()? != "data" {
        return None;
    }
    data.parent().map(Path::to_path_buf)
}

/// Puts the page on the Orbit UI service.
struct UiPublisher {
    ui: UiClient,
}

impl PagePublisher for UiPublisher {
    fn register(&self, page: &str) -> std::result::Result<(), String> {
        self.ui
            .register_page(page)
            .map_err(|error| format!("register_page failed with {}", error.code()))
    }

    fn update(&self, page_id: &str, patch: &str) -> std::result::Result<(), String> {
        self.ui
            .update_page(page_id, patch)
            .map_err(|error| format!("update_page failed with {}", error.code()))
    }
}

/// The UI's action callback for the market page. It runs on the UI delivery
/// thread, so it only queues the action.
unsafe extern "system" fn on_market_action(_user: *mut c_void, message: *const u8, len: usize) {
    // Safety: the UI service passes a message of `len` bytes that lives for
    // the duration of the call.
    let bytes = unsafe { std::slice::from_raw_parts(message, len) };
    let Some(action) = std::str::from_utf8(bytes).ok().and_then(Action::parse) else {
        return;
    };
    if let Some(sender) = ACTIONS.get() {
        let _ = sender.send(action);
    }
}

#[derive(Default)]
struct MarketPlugin;

impl Plugin for MarketPlugin {
    fn init(&mut self, cx: &Context) -> Result {
        info!(cx, "market {} starting", env!("CARGO_PKG_VERSION"));

        let Some(root) = cx
            .data_dir()
            .and_then(|dir| root_of_data_dir(Path::new(dir), cx.plugin_id()))
        else {
            warn!(
                cx,
                "the data folder is not <root>/data/{}, the market stays idle",
                cx.plugin_id()
            );
            return Ok(());
        };

        // The page is the only thing the market has to show; without the UI
        // service there is nowhere to show it, so nothing is started.
        let Some(ui) = UiClient::new(cx) else {
            warn!(
                cx,
                "parsec-orbit.ui is not available, the market stays idle"
            );
            return Ok(());
        };

        let (sender, actions) = channel();
        if ACTIONS.set(sender).is_err() {
            warn!(cx, "the market is already running in this process");
            return Ok(());
        }

        let sources = match sources::load_sources(&root.join("market")) {
            Ok(sources) => sources,
            Err(error) => {
                warn!(cx, "market sources: {error}");
                Vec::new()
            }
        };
        let worker = Worker::new(
            root,
            sources,
            PlatformId::current(),
            Box::new(Real),
            UiPublisher { ui },
        );
        std::thread::Builder::new()
            .name(format!("{}:market", cx.plugin_id()))
            .spawn(move || worker.run(actions))?;

        if let Err(error) = unsafe { ui.on_action(PAGE_ID, on_market_action, std::ptr::null_mut()) }
        {
            warn!(
                cx,
                "the market page does not report its actions: {}",
                error.code()
            );
        }
        Ok(())
    }
}

parsec_orbit_plugin_sdk::declare_plugin! {
    id = "com.parsec-orbit.market",
    name = "Market",
    version = "0.1.1",
    author = "Parsec Orbit contributors",
    description = "Browse, install, update and remove plugins from the signed registries",
    license = "MIT",
    plugin = MarketPlugin,
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_plugin_sdk::{
        MessageCallback, ParsecOrbitInterfaceHeader, ParsecOrbitUiV2, ABI_VERSION,
    };
    use parsec_orbit_plugin_sdk_testing::MockHost;
    use std::ffi::CStr;
    use std::sync::Mutex;

    const ID: &str = "com.parsec-orbit.market";

    #[test]
    fn the_orbit_root_is_two_levels_above_the_plugin_data_folder() {
        let root = Path::new("/home/u/ParsecOrbit");
        assert_eq!(
            root_of_data_dir(&root.join("data").join(ID), ID),
            Some(root.to_path_buf())
        );
        // Another plugin's folder, or a folder not named data, is not the shape.
        assert_eq!(root_of_data_dir(&root.join("data").join("other"), ID), None);
        assert_eq!(root_of_data_dir(&root.join("plugins").join(ID), ID), None);
        assert_eq!(root_of_data_dir(Path::new(ID), ID), None);
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
        assert_eq!(manifest_value("role"), "plugin");
        assert_eq!(manifest_value("entry"), "parsec_orbit_plugin_market.dll");
        assert_eq!(manifest_value("version"), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn the_manifest_requires_the_ui_and_takes_the_config_when_present() {
        let manifest = include_str!("../plugin.toml");
        assert!(
            manifest.contains("requires = [\"com.parsec-orbit.ui\"]"),
            "{manifest}"
        );
        assert!(
            manifest.contains("optional = [\"com.parsec-orbit.config\"]"),
            "{manifest}"
        );
    }

    // ---- init ------------------------------------------------------------------------

    /// Without a data folder of the right shape the plugin cannot find its root,
    /// so it logs that and stays idle.
    #[test]
    fn init_stays_idle_without_an_orbit_root() {
        let host = MockHost::new(ID);
        let (_plugin, result) = host.init::<MarketPlugin>();
        assert!(result.is_ok());
        assert!(host.logged("the market stays idle"));
    }

    /// With the right data folder but no UI service, nothing is shown and nothing
    /// is started.
    #[test]
    fn init_stays_idle_without_the_ui_service() {
        let data = std::env::temp_dir()
            .join("ParsecOrbit-test")
            .join("data")
            .join(ID);
        let host = MockHost::new(ID).with_data_dir(&data);
        let (_plugin, result) = host.init::<MarketPlugin>();
        assert!(result.is_ok());
        assert!(host.logged("parsec-orbit.ui is not available"));
    }

    static REGISTERED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static SUBSCRIBED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static PATCHES: Mutex<Vec<String>> = Mutex::new(Vec::new());

    unsafe extern "system" fn fake_register(page: *const u8, len: usize) -> i32 {
        let text = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(page, len) });
        REGISTERED.lock().unwrap().push(text.into_owned());
        0
    }

    unsafe extern "system" fn fake_update_page(
        _page: *const u8,
        _id_len: usize,
        patch: *const u8,
        len: usize,
    ) -> i32 {
        let text = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(patch, len) });
        PATCHES.lock().unwrap().push(text.into_owned());
        0
    }

    unsafe extern "system" fn fake_on_action(
        page: *const u8,
        len: usize,
        _cb: Option<MessageCallback>,
        _user: *mut c_void,
    ) -> i64 {
        let text = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(page, len) });
        SUBSCRIBED.lock().unwrap().push(text.into_owned());
        1
    }

    static FAKE_UI: ParsecOrbitUiV2 = ParsecOrbitUiV2 {
        header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitUiV2>(), 2, 0),
        show: None,
        hide: None,
        is_visible: None,
        navigate: None,
        register_page: Some(fake_register),
        unregister_page: None,
        update_page: Some(fake_update_page),
        on_action: Some(fake_on_action),
        off_action: None,
    };

    /// The plugin registers its page, subscribes to its actions, and the
    /// page it shows starts from the state of an empty market.
    #[test]
    fn init_registers_the_page_and_subscribes_to_it() {
        let data = std::env::temp_dir()
            .join("ParsecOrbit-test-ui")
            .join("data")
            .join(ID);
        let host = MockHost::new(ID).with_data_dir(&data);
        host.provide("parsec-orbit.ui", &FAKE_UI.header);

        let (_plugin, result) = host.init::<MarketPlugin>();
        assert!(result.is_ok());

        let registered = REGISTERED.lock().unwrap();
        assert_eq!(registered.len(), 1, "registered once");
        let page: serde_json::Value = serde_json::from_str(&registered[0]).unwrap();
        assert_eq!(page["id"], PAGE_ID);
        assert_eq!(page["title"], "Market");
        drop(registered);

        assert_eq!(SUBSCRIBED.lock().unwrap().as_slice(), [PAGE_ID]);
        assert!(host.logged("market"), "the start is logged");
        assert!(
            PATCHES
                .lock()
                .unwrap()
                .iter()
                .any(|patch| patch.contains("market/sources.toml")),
            "with no sources listed, the first patch says so"
        );
        assert!(ACTIONS.get().is_some(), "the queue is open");
    }
}
