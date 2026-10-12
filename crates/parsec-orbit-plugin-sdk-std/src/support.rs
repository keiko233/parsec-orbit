//! Conveniences that need `std`.

use std::io;
use std::string::{String, ToString};
use std::thread::{Builder, JoinHandle};

use parsec_orbit_plugin_sdk::{Context, Level};

/// `std` extras for [`Context`].
pub trait ContextExt {
    /// Starts a thread named `<plugin id>:<name>`. `init` runs on the
    /// loader's worker thread and later plugins wait for it, so long work
    /// goes on a thread of its own.
    fn spawn<F>(&self, name: &str, work: F) -> io::Result<JoinHandle<()>>
    where
        F: FnOnce() + Send + 'static;

    /// Writes panics to the loader log, tagged with the plugin id, instead of
    /// the standard error stream the client does not have.
    fn install_panic_hook(&self);
}

impl ContextExt for Context {
    fn spawn<F>(&self, name: &str, work: F) -> io::Result<JoinHandle<()>>
    where
        F: FnOnce() + Send + 'static,
    {
        let id = self.plugin_id();
        let thread_name: String = if id.is_empty() {
            name.to_string()
        } else {
            std::format!("{id}:{name}")
        };
        Builder::new().name(thread_name).spawn(work)
    }

    fn install_panic_hook(&self) {
        let cx = *self;
        let previous = std::panic::take_hook();
        std::panic::set_hook(std::boxed::Box::new(move |info| {
            cx.log(Level::Error, format_args!("panic: {info}"));
            previous(info);
        }));
    }
}
