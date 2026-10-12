//! Smallest useful Parsec Orbit plugin.
//!
//! It shows the three things a plugin normally does: report its descriptor
//! through `parsec_orbit_plugin!`, talk to the loader through `ParsecOrbitHost`, and
//! return 0 from init when everything is fine.

use std::path::PathBuf;

use parsec_orbit_plugin_sdk::{ParsecOrbitHost, LOG_INFO};

const BUILD: u32 = 0x0001;

fn report_path() -> Option<PathBuf> {
    std::env::var("APPDATA")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|dir| {
            PathBuf::from(dir)
                .join("ParsecOrbit")
                .join("hello-plugin.log")
        })
}

unsafe extern "system" fn init(host: *const ParsecOrbitHost) -> i32 {
    let host = unsafe { &*host };
    let loader = host.loader_version().unwrap_or("unknown");

    host.log(
        LOG_INFO,
        &format!("hello-plugin has been loaded by Parsec Orbit {loader}"),
    );

    let Some(path) = report_path() else {
        host.log(LOG_INFO, "APPDATA is not set, skipping the report file");
        return 0;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let report = format!(
        "Parsec Orbit hello-plugin\nbuild={BUILD}\npid={}\nloader={loader}\n",
        std::process::id()
    );
    match std::fs::write(&path, report) {
        Ok(()) => {
            host.log(LOG_INFO, &format!("wrote {}", path.display()));
            0
        }
        Err(error) => {
            host.log(
                parsec_orbit_plugin_sdk::LOG_ERROR,
                &format!("could not write {}: {error}", path.display()),
            );
            1
        }
    }
}

parsec_orbit_plugin_sdk::parsec_orbit_plugin! {
    id = "com.example.hello",
    name = "Hello Plugin",
    version = "0.1.0",
    author = "Parsec Orbit contributors",
    description = "Example plugin that logs and writes a report file",
    license = "MIT",
    init = init,
}
