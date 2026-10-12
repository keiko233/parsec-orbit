//! Typed bindings for host services.
//!
//! A service is a table of functions registered under a name; the
//! [`Interface`](crate::Interface) trait describes how to find one with
//! [`Context::service`](crate::Context::service). This module holds the
//! bindings for the services the loader itself defines. The kind handler
//! below is the one every runtime plugin implements; the WebView and
//! JavaScript services are provided by official plugins and bound next to
//! them.

use crate::abi::{ParsecOrbitInterfaceHeader, ParsecOrbitKindHandlerV1, ParsecOrbitPluginLoadInfo};

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
    MessageCallback, ParsecOrbitJsV1, ParsecOrbitWebViewV1, ERR_INVALID, ERR_NOT_READY,
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
