//! Safe building blocks for Parsec Orbit plugins.
//!
//! The C ABI itself lives in [`parsec_orbit_plugin_abi`] and is re-exported
//! here, both as [`abi`] and at the crate root, so a plugin needs one
//! dependency only.

#![no_std]

#[cfg(test)]
extern crate std;

pub use parsec_orbit_plugin_abi as abi;
pub use parsec_orbit_plugin_abi::*;

/// Declares the exports of a plugin.
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
            host_abi: u32,
        ) -> *const $crate::ParsecOrbitPluginDesc {
            const ID: &str = ::core::concat!($id, "\0");
            const NAME: &str = ::core::concat!($name, "\0");
            const VERSION: &str = ::core::concat!($version, "\0");
            const AUTHOR: &str = ::core::concat!($author, "\0");
            const DESCRIPTION: &str = ::core::concat!($description, "\0");
            const LICENSE: &str = ::core::concat!($license, "\0");

            static DESC: $crate::SyncDesc = $crate::SyncDesc($crate::ParsecOrbitPluginDesc {
                abi_version: $crate::ABI_VERSION,
                struct_size: ::core::mem::size_of::<$crate::ParsecOrbitPluginDesc>() as u32,
                id: ID.as_ptr(),
                name: NAME.as_ptr(),
                version: VERSION.as_ptr(),
                author: AUTHOR.as_ptr(),
                description: DESCRIPTION.as_ptr(),
                license: LICENSE.as_ptr(),
                capabilities: {
                    #[allow(unused_mut, unused_assignments)]
                    let mut capabilities: u64 = 0;
                    $(capabilities = $capabilities;)?
                    capabilities
                },
            });

            if host_abi != $crate::ABI_VERSION {
                // The loader decides what to do with a mismatch; handing back
                // the descriptor lets it explain the problem in its log.
                let _ = host_abi;
            }
            &DESC.0
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
