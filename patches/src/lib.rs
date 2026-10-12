//! Runtime patches for the Parsec client interfaces.
//!
//! The client UI only offers a few built-in choices for some settings even
//! though the configuration system behind it accepts much wider ranges. The
//! patches here widen those lists at runtime, without touching any file on
//! disk:
//!
//! * [`webview`] attaches to the WebView2 DevTools endpoint the loader opens
//!   and injects an extension script into the web UI.
//! * [`legacy`] edits the in-memory option table of the native (libmatoya) UI.

pub mod legacy;
pub mod webview;

/// Bandwidth options appended after the built-in `3..50 mbps` ladder.
pub const EXTRA_BANDWIDTH_MBPS: [i32; 10] = [60, 75, 100, 125, 150, 200, 250, 300, 400, 500];
