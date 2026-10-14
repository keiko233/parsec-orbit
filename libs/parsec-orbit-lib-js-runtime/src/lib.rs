//! JavaScript runtime - official Parsec Orbit plugin.
//!
//! Runs plugins whose manifest says `kind = "js"` on the Boa engine, and lets
//! native plugins evaluate script through `parsec-orbit.js`.
//!
//! * It registers `parsec-orbit.kind.js`: for each JavaScript plugin the loader
//!   hands over, the runtime starts a context on a thread of its own
//!   ([`engine`]), evaluates the entry module, calls the default export's
//!   `setup(host)` and waits for it. What `host` offers - logging, timers, the
//!   webview, a settings file - is in [`host`]; capabilities beyond logging
//!   and timers need a matching entry in the manifest's `permissions`.
//! * It registers `parsec-orbit.js` ([`service`]) for native plugins.
//!
//! Plugins are isolated from each other (one context each) but not from the
//! process: an engine bug is a bug in the client. JavaScript has no file,
//! network or process access except what `host` grants.

pub mod engine;
pub mod host;
pub mod loader;
pub mod service;

use parsec_orbit_plugin_sdk::prelude::*;

#[derive(Default)]
struct JsRuntime;

impl Plugin for JsRuntime {
    fn init(&mut self, cx: &Context) -> Result {
        info!(
            cx,
            "javascript runtime {} starting",
            env!("CARGO_PKG_VERSION")
        );
        loader::configure(*cx);
        cx.register_kind_handler("js", &service::KIND_HANDLER)?;
        cx.register::<parsec_orbit_plugin_sdk::services::Js>(&service::JS)?;
        Ok(())
    }
}

parsec_orbit_plugin_sdk::declare_plugin! {
    id = "com.parsec-orbit.js-runtime",
    name = "JavaScript Runtime",
    version = "0.1.1",
    author = "Parsec Orbit contributors",
    description = "Runs JavaScript plugins (kind = \"js\") and provides parsec-orbit.js",
    license = "MIT",
    plugin = JsRuntime,
}
