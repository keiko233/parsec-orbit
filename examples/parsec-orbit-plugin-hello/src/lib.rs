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

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_plugin_sdk::{ABI_VERSION, LOG_ERROR};
    use std::ffi::CStr;
    use std::sync::Mutex;

    static LOGGED: Mutex<Vec<(u32, String)>> = Mutex::new(Vec::new());

    unsafe extern "system" fn record(level: u32, message: *const u8, len: usize) {
        let bytes = unsafe { std::slice::from_raw_parts(message, len) };
        LOGGED
            .lock()
            .unwrap()
            .push((level, String::from_utf8_lossy(bytes).into_owned()));
    }

    fn host() -> ParsecOrbitHost {
        ParsecOrbitHost {
            log: Some(record),
            loader_version: b"9.9.9\0".as_ptr(),
            ..ParsecOrbitHost::blank()
        }
    }

    fn logged(text: &str) -> bool {
        LOGGED
            .lock()
            .unwrap()
            .iter()
            .any(|(_, message)| message.contains(text))
    }

    fn scratch(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("parsec-orbit-hello-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The only test that touches `APPDATA`, so the three situations run in
    /// sequence.
    #[test]
    fn init_reports_through_the_host_and_writes_the_report_file() {
        let host = host();

        // 1. A usable %APPDATA%: report written, init succeeds.
        let appdata = scratch("ok");
        std::env::set_var("APPDATA", &appdata);
        assert_eq!(unsafe { init(&host) }, 0);
        assert!(logged("hello-plugin has been loaded by Parsec Orbit 9.9.9"));
        let report =
            std::fs::read_to_string(appdata.join("ParsecOrbit").join("hello-plugin.log")).unwrap();
        assert!(
            report.starts_with("Parsec Orbit hello-plugin\n"),
            "{report}"
        );
        assert!(report.contains("build=1\n"), "{report}");
        assert!(
            report.contains(&format!("pid={}\n", std::process::id())),
            "{report}"
        );
        assert!(report.contains("loader=9.9.9\n"), "{report}");

        // 2. No %APPDATA%: nothing to write, still a success.
        std::env::set_var("APPDATA", "");
        assert_eq!(unsafe { init(&host) }, 0);
        assert!(logged("APPDATA is not set, skipping the report file"));

        // 3. %APPDATA% is a file, so the directory cannot be created: the
        // failure is reported to the loader and init returns non-zero.
        let blocked = scratch("blocked");
        let file = blocked.join("not-a-directory");
        std::fs::write(&file, "x").unwrap();
        std::env::set_var("APPDATA", &file);
        assert_eq!(unsafe { init(&host) }, 1);
        assert!(LOGGED
            .lock()
            .unwrap()
            .iter()
            .any(|(level, message)| *level == LOG_ERROR && message.contains("could not write")));

        std::env::remove_var("APPDATA");
        let _ = std::fs::remove_dir_all(appdata);
        let _ = std::fs::remove_dir_all(blocked);
    }

    #[test]
    fn descriptor_matches_the_manifest() {
        let manifest = include_str!("../plugin.toml");
        let value = |key: &str| {
            manifest
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once('=')?;
                    (name.trim() == key).then(|| value.trim().trim_matches('"').to_string())
                })
                .unwrap_or_else(|| panic!("plugin.toml has no `{key}`"))
        };
        let text = |pointer: *const u8| {
            unsafe { CStr::from_ptr(pointer as *const _) }
                .to_str()
                .unwrap()
                .to_string()
        };

        let desc = unsafe { &*ParsecOrbitPluginQuery(ABI_VERSION) };
        assert_eq!(text(desc.id), value("id"));
        assert_eq!(text(desc.name), value("name"));
        assert_eq!(text(desc.version), value("version"));
        assert_eq!(text(desc.description), value("description"));
        assert_eq!(text(desc.license), value("license"));
        assert_eq!(text(desc.author), value("author"));
    }
}
