//! The smallest Parsec Orbit plugin.
//!
//! It is `#![no_std]` and brings no C runtime: [`freestanding!`] supplies the
//! allocator, the panic handler and the few functions compiled code calls
//! into, so the DLL is a few kilobytes where a `std` plugin is over a
//! hundred. The price is that the crate cannot use `std`; see `hello` for the
//! `std` flavour.
//!
//! The crate has to be built with `panic = "abort"` in every profile (see the
//! workspace manifest), because a `no_std` library cannot unwind.
#![cfg_attr(not(test), no_std)]

use parsec_orbit_plugin_sdk::prelude::*;

parsec_orbit_plugin_sdk::freestanding!();

#[derive(Default)]
struct Hello;

impl Plugin for Hello {
    fn init(&mut self, cx: &Context) -> Result {
        info!(
            cx,
            "hello-nostd has been loaded by Parsec Orbit {}",
            cx.loader_version()
        );
        if let Some(dir) = cx.data_dir() {
            debug!(cx, "my data directory would be {dir}");
        }
        if !cx.has_revision(2) {
            warn!(cx, "this loader is older than host revision 2");
        }
        Ok(())
    }

    fn shutdown(&mut self) {}
}

parsec_orbit_plugin_sdk::declare_plugin! {
    id = "com.example.hello-nostd",
    name = "Hello Plugin (no_std)",
    version = "0.1.0",
    author = "Parsec Orbit contributors",
    description = "Smallest possible plugin: no_std and no C runtime",
    license = "MIT",
    plugin = Hello,
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_plugin_sdk::{LOG_DEBUG, LOG_INFO, LOG_WARN};
    use parsec_orbit_plugin_sdk_std::testing::MockHost;
    use std::path::Path;

    #[test]
    fn init_greets_the_loader() {
        let host = MockHost::new("com.example.hello-nostd");
        let (_, result) = host.init::<Hello>();
        assert!(result.is_ok());
        let logs = host.logs();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, LOG_INFO);
        assert_eq!(logs[0].target.as_deref(), Some("com.example.hello-nostd"));
        assert_eq!(
            logs[0].message,
            "hello-nostd has been loaded by Parsec Orbit 0.0.0-mock"
        );
    }

    #[test]
    fn init_mentions_the_data_directory_when_there_is_one() {
        let host = MockHost::new("com.example.hello-nostd").with_data_dir(Path::new("C:/data/x"));
        host.init::<Hello>().1.unwrap();
        assert!(host.logs().iter().any(|line| line.level == LOG_DEBUG
            && line.message == "my data directory would be C:/data/x"));
    }

    #[test]
    fn init_notes_an_old_loader_and_still_succeeds() {
        let host = MockHost::revision_1("ignored");
        assert!(host.init::<Hello>().1.is_ok());
        assert!(host
            .logs()
            .iter()
            .any(|line| line.level == LOG_WARN
                && line.message.contains("older than host revision 2")));
    }
}
