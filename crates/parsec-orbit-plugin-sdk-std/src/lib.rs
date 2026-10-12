//! `std` companions of [`parsec_orbit_plugin_sdk`].
//!
//! The SDK itself is `no_std` and must stay free of `std`: a `no_std` plugin
//! installs its own panic handler, which cannot coexist with the `std` crate
//! anywhere in its dependency graph. Everything that needs `std` lives here
//! instead, so a plugin opts in by depending on this crate:
//!
//! * [`ContextExt`] - threads named after the plugin, a panic hook that logs
//!   to the loader,
//! * [`TracingExt`] (feature `tracing`) - forwards `tracing` events to the
//!   loader log,
//! * [`testing::MockHost`] - runs a plugin's `init` in a unit test, also for
//!   `no_std` plugins (as a dev-dependency).
//!
//! The SDK is re-exported, so a `std` plugin needs this dependency only.

pub use parsec_orbit_plugin_sdk::*;

mod support;
pub mod testing;
#[cfg(feature = "tracing")]
mod tracing_bridge;

pub use support::ContextExt;
#[cfg(feature = "tracing")]
pub use tracing_bridge::TracingExt;

/// Everything a `std` plugin usually imports.
pub mod prelude {
    pub use super::ContextExt;
    #[cfg(feature = "tracing")]
    pub use super::TracingExt;
    pub use parsec_orbit_plugin_sdk::prelude::*;
}
