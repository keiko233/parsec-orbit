//! Typed bindings for host services.
//!
//! A service is a table of functions registered under a name; the
//! [`Interface`](crate::Interface) trait describes how to find one with
//! [`Context::service`](crate::Context::service). This module holds the
//! bindings for the services the loader itself defines. The kind handler
//! below is the one every runtime plugin implements; the WebView and
//! JavaScript services are provided by official plugins and bound next to
//! them.

use crate::abi::{
    ParsecOrbitClientV1, ParsecOrbitInterfaceHeader, ParsecOrbitKindHandlerV1,
    ParsecOrbitPluginLoadInfo,
};

/// Builds the table a plugin registers with
/// [`Context::register_kind_handler`](crate::Context::register_kind_handler)
/// to serve plugins of its kind.
///
/// `load_plugin` runs on the loader's worker thread once the handler and every
/// other dependency of the plugin to load are initialised; it returns `0` for
/// success and logs the reason for any failure itself.
pub const fn kind_handler(
    load_plugin: unsafe extern "system" fn(info: *const ParsecOrbitPluginLoadInfo) -> i32,
    unload_plugin: Option<unsafe extern "system" fn(id: *const u8) -> i32>,
) -> ParsecOrbitKindHandlerV1 {
    ParsecOrbitKindHandlerV1 {
        header: ParsecOrbitInterfaceHeader::new(
            core::mem::size_of::<ParsecOrbitKindHandlerV1>(),
            1,
            0,
        ),
        load_plugin: Some(load_plugin),
        unload_plugin,
    }
}

// ---- parsec-orbit.webview ----------------------------------------------------

use core::ffi::{c_void, CStr};
use core::fmt::Write as _;

use crate::abi::{
    MessageCallback, ParsecOrbitJsV1, ParsecOrbitUiV1, ParsecOrbitWebViewV1, ERR_INVALID,
    ERR_NOT_READY,
};
use crate::context::{Context, Error, Interface, Result};
use crate::text::StackBuf;

/// Binding of `parsec-orbit.webview`, version 1.
pub struct WebView;

impl Interface for WebView {
    const NAME: &'static CStr = c"parsec-orbit.webview";
    const MAJOR: u16 = 1;
    type Table = ParsecOrbitWebViewV1;

    fn header(table: &ParsecOrbitWebViewV1) -> &ParsecOrbitInterfaceHeader {
        &table.header
    }
}

/// Binding of `parsec-orbit.js`, version 1.
pub struct Js;

impl Interface for Js {
    const NAME: &'static CStr = c"parsec-orbit.js";
    const MAJOR: u16 = 1;
    type Table = ParsecOrbitJsV1;

    fn header(table: &ParsecOrbitJsV1) -> &ParsecOrbitInterfaceHeader {
        &table.header
    }
}

/// Binding of `parsec-orbit.client`, version 1.
pub struct Client;

impl Interface for Client {
    const NAME: &'static CStr = c"parsec-orbit.client";
    const MAJOR: u16 = 1;
    type Table = ParsecOrbitClientV1;

    fn header(table: &ParsecOrbitClientV1) -> &ParsecOrbitInterfaceHeader {
        &table.header
    }
}

/// Passes a result string to a Rust closure.
unsafe extern "system" fn collect(user: *mut c_void, message: *const u8, len: usize) {
    let sink = unsafe { &mut *(user as *mut &mut dyn FnMut(&str)) };
    let bytes = unsafe { core::slice::from_raw_parts(message, len) };
    // The host promises UTF-8; anything else is shown as empty rather than
    // trusted.
    sink(core::str::from_utf8(bytes).unwrap_or(""));
}

fn code(result: i32) -> Result {
    if result == 0 {
        Ok(())
    } else {
        Err(Error::new(result))
    }
}

fn id(result: i64) -> Result<i64> {
    if result > 0 {
        Ok(result)
    } else {
        Err(Error::new(result as i32))
    }
}

/// Safe calls on `parsec-orbit.webview`. Script keys are prefixed with the
/// plugin id, because the registry keys are global.
#[derive(Clone, Copy)]
pub struct WebViewClient {
    table: &'static ParsecOrbitWebViewV1,
    plugin_id: &'static str,
}

impl WebViewClient {
    /// The service, if the webview bridge is loaded and speaks version 1.
    pub fn new(cx: &Context) -> Option<WebViewClient> {
        cx.service::<WebView>().map(|table| WebViewClient {
            table,
            plugin_id: cx.plugin_id(),
        })
    }

    /// Whether a page is attached.
    pub fn is_ready(&self) -> bool {
        self.table
            .is_ready
            .is_some_and(|is_ready| unsafe { is_ready() } == 1)
    }

    /// Runs `script` in the current document and in every new one. Calling it
    /// again with the same `key` replaces the script.
    pub fn add_script(&self, key: &str, script: &str) -> Result<i64> {
        let mut full = StackBuf::<256>::new();
        if write!(full, "{}/{}", self.plugin_id, key).is_err() || full.truncated() {
            return Err(Error::with_message(ERR_INVALID, "script key too long"));
        }
        let add = self.table.add_script.ok_or(Error::new(ERR_NOT_READY))?;
        id(unsafe {
            add(
                full.as_str().as_ptr(),
                full.as_str().len(),
                script.as_ptr(),
                script.len(),
            )
        })
    }

    pub fn remove_script(&self, script_id: i64) -> Result {
        let remove = self.table.remove_script.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { remove(script_id) })
    }

    /// Evaluates `expression` in the page; `result` receives the JSON result.
    pub fn evaluate(&self, expression: &str, mut result: impl FnMut(&str)) -> Result {
        let evaluate = self.table.evaluate.ok_or(Error::new(ERR_NOT_READY))?;
        let mut sink: &mut dyn FnMut(&str) = &mut result;
        code(unsafe {
            evaluate(
                expression.as_ptr(),
                expression.len(),
                Some(collect),
                &mut sink as *mut &mut dyn FnMut(&str) as *mut c_void,
            )
        })
    }

    /// Subscribes to what the page sends on `channel`. The callback and its
    /// user pointer have to stay valid until `unsubscribe`.
    ///
    /// # Safety
    /// `callback` may be called from another thread with `user`.
    pub unsafe fn subscribe(
        &self,
        channel: &str,
        callback: MessageCallback,
        user: *mut c_void,
    ) -> Result<i64> {
        let subscribe = self.table.subscribe.ok_or(Error::new(ERR_NOT_READY))?;
        id(unsafe { subscribe(channel.as_ptr(), channel.len(), Some(callback), user) })
    }

    pub fn unsubscribe(&self, subscription: i64) -> Result {
        let unsubscribe = self.table.unsubscribe.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { unsubscribe(subscription) })
    }

    /// Sends `payload` (JSON) to the page's `window.__orbit.on(channel, ...)`.
    pub fn post(&self, channel: &str, payload: &str) -> Result {
        let post = self.table.post.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe {
            post(
                channel.as_ptr(),
                channel.len(),
                payload.as_ptr(),
                payload.len(),
            )
        })
    }
}

/// Safe calls on `parsec-orbit.js`.
#[derive(Clone, Copy)]
pub struct JsClient {
    table: &'static ParsecOrbitJsV1,
}

impl JsClient {
    /// The service, if the JavaScript runtime is loaded and speaks version 1.
    pub fn new(cx: &Context) -> Option<JsClient> {
        cx.service::<Js>().map(|table| JsClient { table })
    }

    /// A named context of your own; `ERR_EXISTS` if the name is taken.
    pub fn create_context(&self, name: &str) -> Result<i64> {
        let create = self.table.create_context.ok_or(Error::new(ERR_NOT_READY))?;
        id(unsafe { create(name.as_ptr(), name.len()) })
    }

    /// Evaluates `source`; `result` receives the value as JSON.
    pub fn eval(&self, context: i64, source: &str, mut result: impl FnMut(&str)) -> Result {
        let eval = self.table.eval.ok_or(Error::new(ERR_NOT_READY))?;
        let mut sink: &mut dyn FnMut(&str) = &mut result;
        code(unsafe {
            eval(
                context,
                source.as_ptr(),
                source.len(),
                Some(collect),
                &mut sink as *mut &mut dyn FnMut(&str) as *mut c_void,
            )
        })
    }

    pub fn destroy_context(&self, context: i64) -> Result {
        let destroy = self
            .table
            .destroy_context
            .ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { destroy(context) })
    }
}

/// Safe calls on `parsec-orbit.client`.
#[derive(Clone, Copy)]
pub struct ClientClient {
    table: &'static ParsecOrbitClientV1,
}

impl ClientClient {
    /// The service, if the loader registered a complete version 1 table.
    pub fn new(cx: &Context) -> Option<ClientClient> {
        cx.service::<Client>().map(|table| ClientClient { table })
    }

    /// Gets client build information as JSON. The callback runs before this
    /// method returns, and its borrowed text is valid only for that call.
    pub fn info(&self, mut out: impl FnMut(&str)) -> Result {
        let info = self.table.info.ok_or(Error::new(ERR_NOT_READY))?;
        let mut sink: &mut dyn FnMut(&str) = &mut out;
        code(unsafe {
            info(
                Some(collect),
                &mut sink as *mut &mut dyn FnMut(&str) as *mut c_void,
            )
        })
    }
}

// ---- parsec-orbit.config -----------------------------------------------------

use crate::abi::{ParsecOrbitConfigV1, ParsecOrbitHooksV1, ERR_NOT_FOUND};

/// Binding of `parsec-orbit.config`, version 1.
pub struct Config;

impl Interface for Config {
    const NAME: &'static CStr = c"parsec-orbit.config";
    const MAJOR: u16 = 1;
    type Table = ParsecOrbitConfigV1;

    fn header(table: &ParsecOrbitConfigV1) -> &ParsecOrbitInterfaceHeader {
        &table.header
    }
}

/// Settings of one plugin, kept by the config plugin. Values are JSON text;
/// the keys are the plugin's own (each plugin sees only its own object).
#[derive(Clone, Copy)]
pub struct ConfigClient {
    table: &'static ParsecOrbitConfigV1,
    owner: &'static str,
}

impl ConfigClient {
    /// The service, if the config plugin is loaded and speaks version 1.
    pub fn new(cx: &Context) -> Option<ConfigClient> {
        cx.service::<Config>().map(|table| ConfigClient {
            table,
            owner: cx.plugin_id(),
        })
    }

    /// The same service on behalf of another owner, for a plugin that keeps
    /// settings for others (the JavaScript runtime does).
    pub fn for_owner(self, owner: &'static str) -> ConfigClient {
        ConfigClient { owner, ..self }
    }

    /// Passes the JSON value of `key` to `value`; `Ok(false)` if it is not set.
    pub fn get(&self, key: &str, mut value: impl FnMut(&str)) -> Result<bool> {
        let get = self.table.get.ok_or(Error::new(ERR_NOT_READY))?;
        let mut sink: &mut dyn FnMut(&str) = &mut value;
        let result = unsafe {
            get(
                self.owner.as_ptr(),
                self.owner.len(),
                key.as_ptr(),
                key.len(),
                Some(collect),
                &mut sink as *mut &mut dyn FnMut(&str) as *mut c_void,
            )
        };
        match result {
            0 => Ok(true),
            ERR_NOT_FOUND => Ok(false),
            other => Err(Error::new(other)),
        }
    }

    /// Stores `json` (valid JSON text) under `key`.
    pub fn set(&self, key: &str, json: &str) -> Result {
        let set = self.table.set.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe {
            set(
                self.owner.as_ptr(),
                self.owner.len(),
                key.as_ptr(),
                key.len(),
                json.as_ptr(),
                json.len(),
            )
        })
    }

    /// Removes `key`; `Ok(false)` if it was not set.
    pub fn remove(&self, key: &str) -> Result<bool> {
        let remove = self.table.remove.ok_or(Error::new(ERR_NOT_READY))?;
        match unsafe {
            remove(
                self.owner.as_ptr(),
                self.owner.len(),
                key.as_ptr(),
                key.len(),
            )
        } {
            0 => Ok(true),
            ERR_NOT_FOUND => Ok(false),
            other => Err(Error::new(other)),
        }
    }

    /// Passes the keys, as a JSON array of strings, to `keys`.
    pub fn keys(&self, mut keys: impl FnMut(&str)) -> Result {
        let list = self.table.keys.ok_or(Error::new(ERR_NOT_READY))?;
        let mut sink: &mut dyn FnMut(&str) = &mut keys;
        code(unsafe {
            list(
                self.owner.as_ptr(),
                self.owner.len(),
                Some(collect),
                &mut sink as *mut &mut dyn FnMut(&str) as *mut c_void,
            )
        })
    }
}

// ---- parsec-orbit.hooks ---------------------------------------------------------

/// Binding of `parsec-orbit.hooks`, version 1.
pub struct Hooks;

impl Interface for Hooks {
    const NAME: &'static CStr = c"parsec-orbit.hooks";
    const MAJOR: u16 = 1;
    type Table = ParsecOrbitHooksV1;

    fn header(table: &ParsecOrbitHooksV1) -> &ParsecOrbitInterfaceHeader {
        &table.header
    }
}

/// Hooks on client functions. The provider chooses the hooking mechanism.
#[derive(Clone, Copy)]
pub struct HooksClient {
    table: &'static ParsecOrbitHooksV1,
}

impl HooksClient {
    /// The service, if the hooks plugin is loaded and speaks version 1.
    pub fn new(cx: &Context) -> Option<HooksClient> {
        cx.service::<Hooks>().map(|table| HooksClient { table })
    }

    /// Address of `symbol` in the loaded module `module` (for example
    /// `"kernel32.dll"`, `"CreateFileW"`).
    pub fn resolve(&self, module: &str, symbol: &str) -> Option<*const c_void> {
        let resolve = self.table.resolve?;
        let address =
            unsafe { resolve(module.as_ptr(), module.len(), symbol.as_ptr(), symbol.len()) };
        (!address.is_null()).then_some(address)
    }

    /// Prepares a hook of `target` that runs `detour`. Returns the hook id and
    /// the pointer that calls the original function. The hook is disabled
    /// until [`HooksClient::enable`]. The returned pointer is valid until
    /// [`HooksClient::remove`], which restores the original bytes and frees
    /// the trampoline the pointer points at.
    ///
    /// # Safety
    /// `target` is the start of a function and `detour` has its exact
    /// signature and calling convention; both stay valid for the process
    /// lifetime.
    pub unsafe fn install(
        &self,
        target: *const c_void,
        detour: *const c_void,
    ) -> Result<(i64, *const c_void)> {
        let install = self.table.install.ok_or(Error::new(ERR_NOT_READY))?;
        let mut original: *const c_void = core::ptr::null();
        id(unsafe { install(target, detour, &mut original) }).map(|hook| (hook, original))
    }

    pub fn enable(&self, hook: i64) -> Result {
        let enable = self.table.enable.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { enable(hook) })
    }

    pub fn disable(&self, hook: i64) -> Result {
        let disable = self.table.disable.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { disable(hook) })
    }

    /// Disables the hook and puts the bytes of the original function back.
    /// The `original` pointer [`HooksClient::install`] returned is dead
    /// afterwards: calling through it is undefined.
    pub fn remove(&self, hook: i64) -> Result {
        let remove = self.table.remove.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { remove(hook) })
    }
}

// ---- parsec-orbit.ui ---------------------------------------------------------

/// Binding of `parsec-orbit.ui`, version 1.
pub struct Ui;

impl Interface for Ui {
    const NAME: &'static CStr = c"parsec-orbit.ui";
    const MAJOR: u16 = 1;
    type Table = ParsecOrbitUiV1;

    fn header(table: &ParsecOrbitUiV1) -> &ParsecOrbitInterfaceHeader {
        &table.header
    }
}

/// Safe calls on `parsec-orbit.ui`.
#[derive(Clone, Copy)]
pub struct UiClient {
    pub(crate) table: &'static ParsecOrbitUiV1,
}

impl UiClient {
    /// The service, if the UI plugin is loaded and speaks version 1.
    pub fn new(cx: &Context) -> Option<UiClient> {
        cx.service::<Ui>().map(|table| UiClient { table })
    }

    /// Builds a client directly from a table (for testing or mocks).
    pub const fn from_table(table: &'static ParsecOrbitUiV1) -> UiClient {
        UiClient { table }
    }

    /// Shows and focuses the companion window.
    pub fn show(&self) -> Result {
        let show = self.table.show.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { show() })
    }

    /// Hides the companion window without stopping the event loop.
    pub fn hide(&self) -> Result {
        let hide = self.table.hide.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { hide() })
    }

    /// Returns `true` if the window is currently visible.
    pub fn is_visible(&self) -> Result<bool> {
        let is_visible = self.table.is_visible.ok_or(Error::new(ERR_NOT_READY))?;
        let res = unsafe { is_visible() };
        if res < 0 {
            Err(Error::new(res))
        } else {
            Ok(res != 0)
        }
    }

    /// Registers a declarative section schema (JSON text).
    pub fn register_section(&self, schema_json: &str) -> Result {
        let register = self
            .table
            .register_section
            .ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { register(schema_json.as_ptr(), schema_json.len()) })
    }

    /// Unregisters a section by its ID.
    pub fn unregister_section(&self, section_id: &str) -> Result {
        let unregister = self
            .table
            .unregister_section
            .ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { unregister(section_id.as_ptr(), section_id.len()) })
    }

    /// Pushes updated state values (JSON object mapping keys to values) for a section.
    pub fn update_state(&self, section_id: &str, state_json: &str) -> Result {
        let update = self.table.update_state.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe {
            update(
                section_id.as_ptr(),
                section_id.len(),
                state_json.as_ptr(),
                state_json.len(),
            )
        })
    }

    /// Subscribes to user interactions within the specified section.
    ///
    /// # Safety
    /// `callback` may be called from the UI delivery thread with `user`. Both must
    /// remain valid until [`UiClient::off_action`].
    pub unsafe fn on_action(
        &self,
        section_id: &str,
        callback: MessageCallback,
        user: *mut c_void,
    ) -> Result<i64> {
        let on = self.table.on_action.ok_or(Error::new(ERR_NOT_READY))?;
        id(unsafe { on(section_id.as_ptr(), section_id.len(), Some(callback), user) })
    }

    /// Cancels an action subscription.
    pub fn off_action(&self, subscription: i64) -> Result {
        let off = self.table.off_action.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { off(subscription) })
    }
}
