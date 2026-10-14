//! A `std` test double for [`parsec_orbit_plugin_sdk`].
//!
//! The SDK is `no_std` and has no features, so the one thing that needs `std`
//! (a per-thread recording host) lives here. Depend on this crate as a
//! **dev-dependency** only, from `std` and `no_std` plugins alike: a plugin
//! that links it into its library would pull `std` into a `no_std` build.

mod testing;

pub use testing::*;
