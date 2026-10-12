//! Running a plugin's `init` in a unit test.
//!
//! [`MockHost`] builds the host table the loader would build for a plugin: it
//! records what the plugin logs, keeps an in-memory interface registry and
//! lets the test choose the paths. The host callbacks are plain functions
//! with no context argument, so the recorded state is kept per test thread;
//! work a plugin does on threads of its own is not recorded.
//!
//! ```
//! use parsec_orbit_plugin_sdk::prelude::*;
//! use parsec_orbit_plugin_sdk_std::testing::MockHost;
//!
//! #[derive(Default)]
//! struct Hello;
//!
//! impl Plugin for Hello {
//!     fn init(&mut self, cx: &Context) -> Result {
//!         info!(cx, "hello from {}", cx.plugin_id());
//!         Ok(())
//!     }
//! }
//!
//! let host = MockHost::new("com.example.hello");
//! let (_plugin, result) = host.init::<Hello>();
//! assert!(result.is_ok());
//! assert!(host.logged("hello from com.example.hello"));
//! ```

use std::boxed::Box;
use std::cell::RefCell;
use std::ffi::{c_void, CStr, CString};
use std::path::Path;
use std::string::{String, ToString};
use std::vec::Vec;

use parsec_orbit_plugin_sdk::abi::{
    ParsecOrbitHost, ParsecOrbitInterfaceHeader, ERR_EXISTS, ERR_INVALID, ERR_NOT_FOUND,
    HOST_REVISION_1_SIZE, OK,
};
use parsec_orbit_plugin_sdk::{Context, Plugin, Result};

/// One line a plugin wrote to the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    /// The ABI level (`LOG_*`).
    pub level: u32,
    /// The target passed to `log_ex`; `None` for plain `log`.
    pub target: Option<String>,
    pub message: String,
}

struct Registered {
    name: String,
    owner: String,
    table: usize,
}

#[derive(Default)]
struct State {
    logs: Vec<LogLine>,
    registry: Vec<Registered>,
}

std::thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| f(&mut state.borrow_mut()))
}

unsafe fn text(pointer: *const u8, len: usize) -> String {
    if pointer.is_null() {
        return String::new();
    }
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(pointer, len) }).into_owned()
}

unsafe extern "system" fn mock_log(level: u32, message: *const u8, len: usize) {
    let message = unsafe { text(message, len) };
    with_state(|state| {
        state.logs.push(LogLine {
            level,
            target: None,
            message,
        })
    });
}

unsafe extern "system" fn mock_log_ex(
    level: u32,
    target: *const u8,
    target_len: usize,
    message: *const u8,
    len: usize,
) {
    let target = unsafe { text(target, target_len) };
    let message = unsafe { text(message, len) };
    with_state(|state| {
        state.logs.push(LogLine {
            level,
            target: Some(target),
            message,
        })
    });
}

unsafe extern "system" fn mock_query(name: *const u8) -> *mut c_void {
    let name = unsafe { CStr::from_ptr(name as *const _) }.to_string_lossy();
    with_state(|state| {
        state
            .registry
            .iter()
            .find(|entry| entry.name == name)
            .map_or(std::ptr::null_mut(), |entry| entry.table as *mut c_void)
    })
}

unsafe extern "system" fn mock_register(
    host: *const ParsecOrbitHost,
    name: *const u8,
    table: *const ParsecOrbitInterfaceHeader,
) -> i32 {
    if host.is_null() || name.is_null() || table.is_null() {
        return ERR_INVALID;
    }
    let owner = unsafe { &*host }.plugin_id().unwrap_or("").to_string();
    let name = unsafe { CStr::from_ptr(name as *const _) }
        .to_string_lossy()
        .into_owned();
    with_state(|state| {
        if state.registry.iter().any(|entry| entry.name == name) {
            return ERR_EXISTS;
        }
        state.registry.push(Registered {
            name,
            owner,
            table: table as usize,
        });
        OK
    })
}

unsafe extern "system" fn mock_unregister(host: *const ParsecOrbitHost, name: *const u8) -> i32 {
    if host.is_null() || name.is_null() {
        return ERR_INVALID;
    }
    let owner = unsafe { &*host }.plugin_id().unwrap_or("").to_string();
    let name = unsafe { CStr::from_ptr(name as *const _) }.to_string_lossy();
    with_state(|state| {
        match state
            .registry
            .iter()
            .position(|entry| entry.name == name && entry.owner == owner)
        {
            Some(index) => {
                state.registry.remove(index);
                OK
            }
            None => ERR_NOT_FOUND,
        }
    })
}

fn leak(text: &str) -> *const u8 {
    CString::new(text.replace('\0', "?"))
        .map(|text| text.into_raw() as *const u8)
        .unwrap_or(std::ptr::null())
}

/// What the mock table is built from.
#[derive(Clone)]
struct Settings {
    id: String,
    plugin_dir: Option<String>,
    data_dir: Option<String>,
    loader_version: String,
    revision_1: bool,
}

/// A stand-in for the loader, for one plugin.
pub struct MockHost {
    settings: Settings,
    host: &'static ParsecOrbitHost,
}

impl MockHost {
    /// A revision 2 host for the plugin `id`. The plugin directory defaults to
    /// the system temporary directory, no data directory is set and the
    /// loader version is `0.0.0-mock`.
    pub fn new(id: &str) -> MockHost {
        MockHost::build(Settings {
            id: id.to_string(),
            plugin_dir: Some(std::env::temp_dir().to_string_lossy().into_owned()),
            data_dir: None,
            loader_version: "0.0.0-mock".to_string(),
            revision_1: false,
        })
    }

    /// What an old loader hands over: a 96 byte table without the revision 2
    /// fields. The plugin still gets `log`.
    pub fn revision_1(id: &str) -> MockHost {
        MockHost::build(Settings {
            id: id.to_string(),
            plugin_dir: None,
            data_dir: None,
            loader_version: "0.0.0-mock".to_string(),
            revision_1: true,
        })
    }

    fn build(settings: Settings) -> MockHost {
        // The table is leaked on purpose: a plugin may keep the pointer for
        // the rest of the process, as it may with the real one.
        let host = Box::leak(Box::new(ParsecOrbitHost {
            struct_size: if settings.revision_1 {
                HOST_REVISION_1_SIZE as u32
            } else {
                std::mem::size_of::<ParsecOrbitHost>() as u32
            },
            log: Some(mock_log),
            query_interface: Some(mock_query),
            loader_version: leak(&settings.loader_version),
            plugin_id: leak(&settings.id),
            plugin_dir: settings
                .plugin_dir
                .as_deref()
                .map_or(std::ptr::null(), leak),
            data_dir: settings.data_dir.as_deref().map_or(std::ptr::null(), leak),
            log_ex: Some(mock_log_ex),
            register_interface: Some(mock_register),
            unregister_interface: Some(mock_unregister),
            ..ParsecOrbitHost::blank()
        }));
        MockHost { settings, host }
    }

    /// Sets the plugin directory.
    pub fn with_plugin_dir(self, dir: &Path) -> MockHost {
        MockHost::build(Settings {
            plugin_dir: Some(dir.to_string_lossy().into_owned()),
            ..self.settings
        })
    }

    /// Sets the data directory.
    pub fn with_data_dir(self, dir: &Path) -> MockHost {
        MockHost::build(Settings {
            data_dir: Some(dir.to_string_lossy().into_owned()),
            ..self.settings
        })
    }

    /// Sets the loader version the plugin sees.
    pub fn with_loader_version(self, version: &str) -> MockHost {
        MockHost::build(Settings {
            loader_version: version.to_string(),
            ..self.settings
        })
    }

    /// The raw table, as passed to `ParsecOrbitPluginInit`.
    pub fn host(&self) -> &'static ParsecOrbitHost {
        self.host
    }

    /// The table wrapped as the plugin sees it.
    pub fn context(&self) -> Context {
        unsafe { Context::from_host(self.host) }.expect("the mock table is large enough")
    }

    /// Creates the plugin and runs its `init`, like the loader does.
    pub fn init<P: Plugin>(&self) -> (P, Result) {
        let mut plugin = P::default();
        let result = plugin.init(&self.context());
        (plugin, result)
    }

    /// Everything logged on this thread so far.
    pub fn logs(&self) -> Vec<LogLine> {
        with_state(|state| state.logs.clone())
    }

    /// Whether any logged message contains `needle`.
    pub fn logged(&self, needle: &str) -> bool {
        with_state(|state| state.logs.iter().any(|line| line.message.contains(needle)))
    }

    /// Forgets the recorded log lines.
    pub fn clear_logs(&self) {
        with_state(|state| state.logs.clear());
    }

    /// Registers `table` under `name` as another plugin would have, so the
    /// plugin under test can look it up.
    pub fn provide(&self, name: &str, table: &'static ParsecOrbitInterfaceHeader) {
        with_state(|state| {
            state.registry.retain(|entry| entry.name != name);
            state.registry.push(Registered {
                name: name.to_string(),
                owner: "mock".to_string(),
                table: table as *const ParsecOrbitInterfaceHeader as usize,
            });
        });
    }

    /// Whether a table is registered under `name`.
    pub fn registered(&self, name: &str) -> bool {
        with_state(|state| state.registry.iter().any(|entry| entry.name == name))
    }

    /// Who registered `name`.
    pub fn owner_of(&self, name: &str) -> Option<String> {
        with_state(|state| {
            state
                .registry
                .iter()
                .find(|entry| entry.name == name)
                .map(|entry| entry.owner.clone())
        })
    }
}
