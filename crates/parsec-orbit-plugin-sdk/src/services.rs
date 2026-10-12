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
