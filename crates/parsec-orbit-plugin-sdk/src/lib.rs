//! Safe building blocks for Parsec Orbit plugins.
//!
//! A plugin is a type that implements [`Plugin`], exported with
//! [`declare_plugin!`]:
//!
//! ```ignore
//! // Not compiled as a doctest: it needs a `no_std` crate with `panic = "abort"`.
//! // See `examples/parsec-orbit-plugin-hello-nostd` for the real thing.
//! #![no_std]
//! use parsec_orbit_plugin_sdk::prelude::*;
//!
//! parsec_orbit_plugin_sdk::freestanding!();
//!
//! #[derive(Default)]
//! struct Hello;
//!
//! impl Plugin for Hello {
//!     fn init(&mut self, cx: &Context) -> Result {
//!         info!(cx, "loaded by Parsec Orbit {}", cx.loader_version());
//!         Ok(())
//!     }
//! }
//!
//! parsec_orbit_plugin_sdk::declare_plugin! {
//!     id = "com.example.hello",
//!     name = "Hello Plugin",
//!     version = "0.1.0",
//!     author = "Example",
//!     description = "Minimal plugin",
//!     license = "MIT",
//!     plugin = Hello,
//! }
//! ```
//!
//! The C ABI itself lives in [`parsec_orbit_plugin_abi`] and is re-exported
//! here, both as [`abi`] and at the crate root, so a plugin needs one
//! dependency only.
//!
//! # No `std`, ever
//!
//! This crate never links `std`, not even behind a feature: a `no_std`
//! plugin installs its own panic handler ([`freestanding!`]), and that cannot
//! coexist with `std` anywhere in the plugin's dependency graph - and Cargo
//! unifies features across a build. What needs `std` (threads, the `tracing`
//! bridge, `testing::MockHost`) is in `parsec-orbit-plugin-sdk-std`.

#![no_std]

#[cfg(test)]
extern crate std;

pub use parsec_orbit_plugin_abi as abi;
pub use parsec_orbit_plugin_abi::*;

mod context;
pub mod plugin;
pub mod runtime;
pub mod services;
mod text;

pub use context::{Context, Error, Interface, Level, Result};
pub use plugin::Plugin;
pub use text::StackBuf;

/// Everything a plugin usually imports.
pub mod prelude {
    pub use crate::{
        debug, declare_plugin, error, info, trace, warn, Context, Error, Interface, Level, Plugin,
        Result,
    };
}

/// Logs at trace level through a [`Context`]: `trace!(cx, "{} things", n)`.
#[macro_export]
macro_rules! trace {
    ($cx:expr, $($arg:tt)+) => {
        $crate::Context::log(&$cx, $crate::Level::Trace, ::core::format_args!($($arg)+))
    };
}

/// Logs at debug level through a [`Context`]: `debug!(cx, "{} things", n)`.
#[macro_export]
macro_rules! debug {
    ($cx:expr, $($arg:tt)+) => {
        $crate::Context::log(&$cx, $crate::Level::Debug, ::core::format_args!($($arg)+))
    };
}

/// Logs at info level through a [`Context`]: `info!(cx, "{} things", n)`.
#[macro_export]
macro_rules! info {
    ($cx:expr, $($arg:tt)+) => {
        $crate::Context::log(&$cx, $crate::Level::Info, ::core::format_args!($($arg)+))
    };
}

/// Logs at warn level through a [`Context`]: `warn!(cx, "{} things", n)`.
#[macro_export]
macro_rules! warn {
    ($cx:expr, $($arg:tt)+) => {
        $crate::Context::log(&$cx, $crate::Level::Warn, ::core::format_args!($($arg)+))
    };
}

/// Logs at error level through a [`Context`]: `error!(cx, "{} things", n)`.
#[macro_export]
macro_rules! error {
    ($cx:expr, $($arg:tt)+) => {
        $crate::Context::log(&$cx, $crate::Level::Error, ::core::format_args!($($arg)+))
    };
}

/// Declares the exports of a plugin written against the raw ABI.
///
/// Superseded by [`declare_plugin!`], which also owns the plugin value and
/// hands `init` a [`Context`]. Kept for one release so existing plugins keep
/// building.
///
/// ```no_run
/// use parsec_orbit_plugin_sdk::{ParsecOrbitHost, LOG_INFO};
///
/// unsafe extern "system" fn init(host: *const ParsecOrbitHost) -> i32 {
///     let host = unsafe { &*host };
///     host.log(LOG_INFO, "hello from a plugin");
///     0
/// }
///
/// # #[allow(deprecated)]
/// parsec_orbit_plugin_sdk::parsec_orbit_plugin! {
///     id = "com.example.hello",
///     name = "Hello",
///     version = "0.1.0",
///     author = "Example",
///     description = "Minimal plugin",
///     license = "MIT",
///     init = init,
/// }
/// ```
#[deprecated(note = "use `declare_plugin!` with a `Plugin` implementation")]
#[macro_export]
macro_rules! parsec_orbit_plugin {
    (
        id = $id:literal,
        name = $name:literal,
        version = $version:literal,
        author = $author:literal,
        description = $description:literal,
        license = $license:literal,
        init = $init:path
        $(, shutdown = $shutdown:path)?
        $(, capabilities = $capabilities:expr)?
        $(,)?
    ) => {
        /// Reports the plugin description to the loader.
        #[no_mangle]
        pub unsafe extern "system" fn ParsecOrbitPluginQuery(
            _host_abi: u32,
        ) -> *const $crate::ParsecOrbitPluginDesc {
            $crate::__descriptor!($id, $name, $version, $author, $description, $license
                $(, $capabilities)?)
        }

        /// Loader entry point; runs on the loader's worker thread.
        #[no_mangle]
        pub unsafe extern "system" fn ParsecOrbitPluginInit(
            host: *const $crate::ParsecOrbitHost,
        ) -> i32 {
            $init(host)
        }

        $(
            /// Optional teardown entry point.
            #[no_mangle]
            pub unsafe extern "system" fn ParsecOrbitPluginShutdown() -> i32 {
                $shutdown()
            }
        )?
    };
}

#[cfg(test)]
mod tests;
