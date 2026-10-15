//! The plugin object and the glue `declare_plugin!` generates around it.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::abi::{ParsecOrbitHost, ERR_EXISTS, ERR_INVALID};
use crate::context::{Context, Error, Result};
use crate::runtime;

/// A Parsec Orbit plugin.
///
/// The value is created with [`Default`] when the loader calls `init`, lives
/// in a static for the rest of the process and is never moved. `init` and
/// `shutdown` run on the loader's worker thread, one after the other.
pub trait Plugin: Default + Send + 'static {
    /// Called once, after the plugin's dependencies finished their own
    /// `init`. Returning `Err` marks the plugin as failed. Do not block: the
    /// loader initialises plugins one by one; start a thread for long work
    /// (`std::thread::Builder`, named after the plugin).
    fn init(&mut self, cx: &Context) -> Result;

    /// Called when the plugin is unloaded. Today nothing unloads plugins, so
    /// this is only reached by tools and tests.
    fn shutdown(&mut self) {}
}

const EMPTY: u8 = 0;
const LIVE: u8 = 1;
const GONE: u8 = 2;

/// Storage for the one plugin value of a module.
#[doc(hidden)]
pub struct Slot<T> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<T>>,
}

// Safety: the state flag keeps the `&mut` unique, and `T: Send` keeps the
// value safe to move to the thread that calls `shutdown` in the future.
unsafe impl<T: Send> Sync for Slot<T> {}

impl<T: Default> Slot<T> {
    pub const fn new() -> Self {
        Slot {
            state: AtomicU8::new(EMPTY),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Creates the value; `None` if there already is one (or was).
    #[allow(clippy::mut_from_ref)] // the state flag makes the borrow unique
    fn create(&self) -> Option<&mut T> {
        self.state
            .compare_exchange(EMPTY, LIVE, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        // Safety: the state flag made this the only writer.
        Some(unsafe { (*self.value.get()).write(T::default()) })
    }

    /// Runs `f` on the live value and then drops it. `None` if the value
    /// does not exist.
    fn retire(&self, f: impl FnOnce(&mut T)) -> Option<()> {
        self.state
            .compare_exchange(LIVE, GONE, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        // Safety: the state flag says the value is live, and it is retired
        // exactly once.
        unsafe {
            let value = (*self.value.get()).assume_init_mut();
            f(value);
            (*self.value.get()).assume_init_drop();
        }
        Some(())
    }
}

impl<T: Default> Default for Slot<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Body of the generated `ParsecOrbitPluginInit`.
///
/// # Safety
/// `host` is the table the loader passed to `ParsecOrbitPluginInit`.
#[doc(hidden)]
pub unsafe fn init<T: Plugin>(slot: &Slot<T>, host: *const ParsecOrbitHost) -> i32 {
    // The SDK needs at least the revision 1 fields (`log`); a table smaller
    // than that is not from a loader this SDK can talk to.
    let Some(cx) = (unsafe { Context::from_host(host) }) else {
        return ERR_INVALID;
    };
    runtime::remember_host(cx.host());
    let Some(plugin) = slot.create() else {
        crate::error!(cx, "init called twice");
        return ERR_EXISTS;
    };
    match plugin.init(&cx) {
        Ok(()) => 0,
        Err(error) => {
            crate::error!(cx, "init failed: {error}");
            error.code()
        }
    }
}

/// Body of the generated `ParsecOrbitPluginShutdown`.
#[doc(hidden)]
pub fn shutdown<T: Plugin>(slot: &Slot<T>) -> i32 {
    match slot.retire(T::shutdown) {
        Some(()) => 0,
        None => Error::new(ERR_INVALID).code(),
    }
}

/// Declares the exports of a plugin.
///
/// ```no_run
/// use parsec_orbit_plugin_sdk::prelude::*;
///
/// #[derive(Default)]
/// struct Hello;
///
/// impl Plugin for Hello {
///     fn init(&mut self, cx: &Context) -> Result {
///         info!(cx, "loaded by Parsec Orbit {}", cx.loader_version());
///         Ok(())
///     }
/// }
///
/// parsec_orbit_plugin_sdk::declare_plugin! {
///     id = "com.example.hello",
///     name = "Hello Plugin",
///     version = "0.1.0",
///     author = "Example",
///     description = "Minimal plugin",
///     license = "MIT",
///     plugin = Hello,
/// }
/// ```
///
/// Generates `ParsecOrbitPluginQuery`, `ParsecOrbitPluginInit` and
/// `ParsecOrbitPluginShutdown`. `capabilities = <u64>` may follow `plugin`.
#[macro_export]
macro_rules! declare_plugin {
    (
        id = $id:literal,
        name = $name:literal,
        version = $version:literal,
        author = $author:literal,
        description = $description:literal,
        license = $license:literal,
        plugin = $plugin:ty
        $(, capabilities = $capabilities:expr)?
        $(,)?
    ) => {
        static __PARSEC_ORBIT_PLUGIN: $crate::plugin::Slot<$plugin> = $crate::plugin::Slot::new();

        /// Reports the plugin description to the loader.
        #[no_mangle]
        pub unsafe extern "system" fn ParsecOrbitPluginQuery(
            _host_abi: u32,
        ) -> *const $crate::ParsecOrbitPluginDesc {
            // The loader decides what to do with a mismatch; handing the
            // descriptor back lets it explain the problem in its log.
            $crate::__descriptor!($id, $name, $version, $author, $description, $license
                $(, $capabilities)?)
        }

        /// Loader entry point; runs on the loader's worker thread.
        #[no_mangle]
        pub unsafe extern "system" fn ParsecOrbitPluginInit(
            host: *const $crate::ParsecOrbitHost,
        ) -> i32 {
            unsafe { $crate::plugin::init::<$plugin>(&__PARSEC_ORBIT_PLUGIN, host) }
        }

        /// Teardown entry point.
        #[no_mangle]
        pub unsafe extern "system" fn ParsecOrbitPluginShutdown() -> i32 {
            $crate::plugin::shutdown::<$plugin>(&__PARSEC_ORBIT_PLUGIN)
        }

    };
}

/// A `static` descriptor built from literals; shared by both export macros.
#[doc(hidden)]
#[macro_export]
macro_rules! __descriptor {
    ($id:literal, $name:literal, $version:literal, $author:literal, $description:literal,
     $license:literal $(, $capabilities:expr)?) => {{
        static DESC: $crate::SyncDesc = $crate::SyncDesc($crate::ParsecOrbitPluginDesc {
            abi_version: $crate::ABI_VERSION,
            struct_size: ::core::mem::size_of::<$crate::ParsecOrbitPluginDesc>() as u32,
            id: ::core::concat!($id, "\0").as_ptr(),
            name: ::core::concat!($name, "\0").as_ptr(),
            version: ::core::concat!($version, "\0").as_ptr(),
            author: ::core::concat!($author, "\0").as_ptr(),
            description: ::core::concat!($description, "\0").as_ptr(),
            license: ::core::concat!($license, "\0").as_ptr(),
            capabilities: {
                #[allow(unused_mut, unused_assignments)]
                let mut capabilities: u64 = 0;
                $(capabilities = $capabilities;)?
                capabilities
            },
        });
        &DESC.0 as *const $crate::ParsecOrbitPluginDesc
    }};
}
