//! Hooks - official Parsec Orbit plugin.
//!
//! The Windows backend uses inline detours. The arm64 macOS backend rebinds
//! imported function pointers only. Other targets expose the service and
//! report that installation is not ready.

#[cfg(windows)]
#[path = "windows.rs"]
mod platform;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[path = "macos.rs"]
mod platform;

#[cfg(not(any(windows, all(target_os = "macos", target_arch = "aarch64"))))]
#[path = "unsupported.rs"]
mod platform;

pub use platform::HOOKS;

#[cfg(windows)]
pub use platform::{disable_hook, enable_hook, install_hook, remove_hook, resolve_symbol};
