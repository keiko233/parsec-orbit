//! Smallest useful Parsec Orbit plugin that uses `std`.
//!
//! It shows what a plugin normally does: describe itself with
//! `declare_plugin!`, talk to the loader through the [`Context`] that `init`
//! receives, and return `Ok` when everything is fine. This one writes a small
//! report file, which needs `std`; see `hello-nostd` for a plugin that is
//! a tenth of the size.

use std::path::PathBuf;

use parsec_orbit_plugin_sdk_std::prelude::*;

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

#[derive(Default)]
struct Hello;

impl Plugin for Hello {
    fn init(&mut self, cx: &Context) -> Result {
        let loader = cx.loader_version();

        info!(cx, "hello-plugin has been loaded by Parsec Orbit {loader}");

        let Some(path) = report_path() else {
            info!(cx, "APPDATA is not set, skipping the report file");
            return Ok(());
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
                info!(cx, "wrote {}", path.display());
                Ok(())
            }
            Err(error) => {
                error!(cx, "could not write {}: {error}", path.display());
                Err(Error::with_message(1, "could not write the report file"))
            }
        }
    }
}

parsec_orbit_plugin_sdk_std::declare_plugin! {
    id = "com.example.hello",
    name = "Hello Plugin",
    version = "0.1.0",
    author = "Parsec Orbit contributors",
    description = "Example plugin that logs and writes a report file",
    license = "MIT",
    plugin = Hello,
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_plugin_sdk_std::testing::MockHost;
    use parsec_orbit_plugin_sdk_std::{ABI_VERSION, LOG_ERROR};
    use std::ffi::CStr;

    fn host() -> MockHost {
        MockHost::new("com.example.hello").with_loader_version("9.9.9")
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
        assert!(host.init::<Hello>().1.is_ok());
        assert!(host.logged("hello-plugin has been loaded by Parsec Orbit 9.9.9"));
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
        assert!(host.init::<Hello>().1.is_ok());
        assert!(host.logged("APPDATA is not set, skipping the report file"));

        // 3. %APPDATA% is a file, so the directory cannot be created: the
        // failure is reported to the loader and init returns non-zero.
        let blocked = scratch("blocked");
        let file = blocked.join("not-a-directory");
        std::fs::write(&file, "x").unwrap();
        std::env::set_var("APPDATA", &file);
        assert_eq!(host.init::<Hello>().1.unwrap_err().code(), 1);
        assert!(host
            .logs()
            .iter()
            .any(|line| line.level == LOG_ERROR && line.message.contains("could not write")));

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
