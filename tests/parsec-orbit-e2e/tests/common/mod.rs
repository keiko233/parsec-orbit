//! Builds the artifacts once per test binary and runs them in sandboxes.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

pub struct Artifacts {
    pub loader: PathBuf,
    pub hello: PathBuf,
    pub webview: PathBuf,
    pub bridge: PathBuf,
    pub native: PathBuf,
    pub fixture: PathBuf,
    pub fixture_nostd: PathBuf,
    pub hello_nostd: PathBuf,
    pub callpaths: PathBuf,
    pub roundtrip: PathBuf,
}

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tests/parsec-orbit-e2e sits two levels below the workspace")
        .to_path_buf()
}

/// Builds the loader (with the `diagnostics` feature, which only adds one
/// export), the plugins and the verification tools into a private target
/// directory, so the nested cargo never waits for the lock of the cargo that
/// runs these tests.
pub fn artifacts() -> &'static Artifacts {
    static ARTIFACTS: OnceLock<Artifacts> = OnceLock::new();
    ARTIFACTS.get_or_init(|| {
        let root = workspace_root();
        let target = root.join("target").join("e2e");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

        let output = Command::new(cargo)
            .current_dir(&root)
            .args(["build", "--target-dir"])
            .arg(&target)
            .args([
                "-p",
                "parsec-orbit-loader",
                "--features",
                "parsec-orbit-loader/diagnostics",
            ])
            .args(["-p", "parsec-orbit-plugin-hello"])
            .args(["-p", "parsec-orbit-plugin-high-bitrate-webview"])
            .args(["-p", "parsec-orbit-plugin-webview-bridge"])
            .args(["-p", "parsec-orbit-plugin-high-bitrate-native"])
            .args(["-p", "parsec-orbit-fixture-plugin"])
            .args(["-p", "parsec-orbit-fixture-nostd"])
            .args(["-p", "parsec-orbit-plugin-hello-nostd"])
            .args(["-p", "parsec-orbit-verify-callpaths"])
            .args(["-p", "parsec-orbit-verify-roundtrip"])
            .output()
            .expect("start cargo");
        assert!(
            output.status.success(),
            "building the artifacts failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let debug = target.join("debug");
        let artifacts = Artifacts {
            loader: debug.join("bcrypt.dll"),
            hello: debug.join("parsec_orbit_plugin_hello.dll"),
            webview: debug.join("parsec_orbit_plugin_high_bitrate_webview.dll"),
            bridge: debug.join("parsec_orbit_plugin_webview_bridge.dll"),
            native: debug.join("parsec_orbit_plugin_high_bitrate_native.dll"),
            fixture: debug.join("parsec_orbit_fixture_plugin.dll"),
            fixture_nostd: debug.join("parsec_orbit_fixture_nostd.dll"),
            hello_nostd: debug.join("parsec_orbit_plugin_hello_nostd.dll"),
            callpaths: debug.join("callpaths.exe"),
            roundtrip: debug.join("roundtrip.exe"),
        };
        for path in [
            &artifacts.loader,
            &artifacts.hello,
            &artifacts.webview,
            &artifacts.bridge,
            &artifacts.native,
            &artifacts.fixture,
            &artifacts.fixture_nostd,
            &artifacts.hello_nostd,
            &artifacts.callpaths,
            &artifacts.roundtrip,
        ] {
            assert!(path.is_file(), "{} was not built", path.display());
        }
        artifacts
    })
}

/// Manifest of an in-repo plugin.
pub fn plugin_manifest(kind: Plugin) -> PathBuf {
    let root = workspace_root();
    match kind {
        Plugin::Hello => root.join("examples/parsec-orbit-plugin-hello/plugin.toml"),
        Plugin::Webview => {
            root.join("plugins/parsec-orbit-plugin-high-bitrate-webview/plugin.toml")
        }
        Plugin::Native => root.join("plugins/parsec-orbit-plugin-high-bitrate-native/plugin.toml"),
        Plugin::WebviewBridge => {
            root.join("plugins/parsec-orbit-plugin-webview-bridge/plugin.toml")
        }
        Plugin::Fixture => root.join("tests/parsec-orbit-fixture-plugin/plugin.toml"),
        Plugin::FixtureNostd => root.join("tests/parsec-orbit-fixture-nostd/plugin.toml"),
        Plugin::HelloNostd => root.join("examples/parsec-orbit-plugin-hello-nostd/plugin.toml"),
    }
}

#[derive(Clone, Copy)]
pub enum Plugin {
    Hello,
    Webview,
    WebviewBridge,
    Native,
    Fixture,
    FixtureNostd,
    HelloNostd,
}

impl Plugin {
    pub fn dll(self) -> &'static Path {
        match self {
            Plugin::Hello => &artifacts().hello,
            Plugin::Webview => &artifacts().webview,
            Plugin::WebviewBridge => &artifacts().bridge,
            Plugin::Native => &artifacts().native,
            Plugin::Fixture => &artifacts().fixture,
            Plugin::FixtureNostd => &artifacts().fixture_nostd,
            Plugin::HelloNostd => &artifacts().hello_nostd,
        }
    }
}

/// A fake client installation plus the user's `%APPDATA%` and `%TEMP%`, all
/// under one temporary directory.
pub struct Sandbox {
    pub root: PathBuf,
    pub client: PathBuf,
    pub appdata: PathBuf,
    pub temp: PathBuf,
    pub log: PathBuf,
}

pub struct Run {
    pub output: Output,
    pub stdout: String,
    pub log: String,
}

impl Sandbox {
    pub fn new(label: &str) -> Sandbox {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "parsec-orbit-e2e-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let sandbox = Sandbox {
            client: root.join("client"),
            appdata: root.join("appdata"),
            temp: root.join("temp"),
            log: root.join("orbit.log"),
            root,
        };
        for dir in [&sandbox.client, &sandbox.appdata, &sandbox.temp] {
            std::fs::create_dir_all(dir).unwrap();
        }
        sandbox
    }

    /// Puts the loader next to the host, the way it sits next to the client.
    pub fn install_loader(&self) -> &Sandbox {
        std::fs::copy(&artifacts().loader, self.client.join("bcrypt.dll")).unwrap();
        self
    }

    /// Copies the host binary into the client directory under `name`.
    pub fn install_host(&self, name: &str) -> &Sandbox {
        std::fs::copy(env!("CARGO_BIN_EXE_loader-host"), self.client.join(name)).unwrap();
        self
    }

    /// `<client>\plugins\<dir>\plugin.toml` + the DLL named by `entry`.
    pub fn install_plugin(&self, dir: &str, plugin: Plugin) -> PathBuf {
        let target = self.client.join("plugins").join(dir);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::copy(plugin_manifest(plugin), target.join("plugin.toml")).unwrap();
        let dll = plugin.dll();
        std::fs::copy(dll, target.join(dll.file_name().unwrap())).unwrap();
        target
    }

    /// A fixture plugin in `<client>\plugins\<dir>` with its own manifest
    /// (`entry` is added) and the script it follows (`fixture.cfg`).
    pub fn install_fixture(&self, dir: &str, manifest: &str, script: &str) -> PathBuf {
        let target = self.client.join("plugins").join(dir);
        std::fs::create_dir_all(&target).unwrap();
        let dll = Plugin::Fixture.dll();
        std::fs::copy(dll, target.join(dll.file_name().unwrap())).unwrap();
        std::fs::write(
            target.join("plugin.toml"),
            format!(
                "{manifest}
entry = \"{}\"
",
                dll.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        std::fs::write(target.join("fixture.cfg"), script).unwrap();
        target
    }

    /// The no_std fixture in `<client>\plugins\<dir>` under the given plugin id.
    pub fn install_nostd_fixture(&self, dir: &str, id: &str) -> PathBuf {
        let target = self.client.join("plugins").join(dir);
        std::fs::create_dir_all(&target).unwrap();
        let dll = Plugin::FixtureNostd.dll();
        std::fs::copy(dll, target.join(dll.file_name().unwrap())).unwrap();
        std::fs::write(
            target.join("plugin.toml"),
            format!(
                "id = \"{id}\"
entry = \"{}\"
",
                dll.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        target
    }

    /// Writes an arbitrary file below the client's `plugins` directory.
    pub fn write_plugin_file(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.client.join("plugins").join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// Runs `<client>\<host>` against the installed loader and waits until the
    /// log at `PARSEC_ORBIT_LOG` contains `wait_for`.
    pub fn run(&self, host: &str, wait_for: &str, customize: impl FnOnce(&mut Command)) -> Run {
        self.run_watching(host, &self.log, wait_for, customize)
    }

    /// Like [`Sandbox::run`], but the host watches `log` instead; for tests
    /// where the loader is steered away from `PARSEC_ORBIT_LOG`.
    pub fn run_watching(
        &self,
        host: &str,
        log: &Path,
        wait_for: &str,
        customize: impl FnOnce(&mut Command),
    ) -> Run {
        let mut command = Command::new(self.client.join(host));
        command
            .arg(self.client.join("bcrypt.dll"))
            .arg(log)
            .arg(wait_for)
            .arg("20");
        self.base_environment(&mut command);
        customize(&mut command);
        self.finish(command, log)
    }

    /// The environment of a clean user session: nothing inherited that could
    /// steer the loader, and every user location redirected into the sandbox.
    pub fn base_environment(&self, command: &mut Command) {
        for (name, _) in std::env::vars_os() {
            let upper = name.to_string_lossy().to_ascii_uppercase();
            if upper.starts_with("PARSEC_ORBIT_") || upper.starts_with("WEBVIEW2_") {
                command.env_remove(name);
            }
        }
        command
            .env("APPDATA", &self.appdata)
            .env("TEMP", &self.temp)
            .env("TMP", &self.temp)
            .env("PARSEC_ORBIT_LOG", &self.log)
            .env("PARSEC_ORBIT_DEVTOOLS", "off");
    }

    /// Runs the prepared host command and collects the log at `log`.
    pub fn finish(&self, mut command: Command, log: &Path) -> Run {
        let output = command.output().expect("start the host process");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let log = std::fs::read_to_string(log).unwrap_or_default();
        Run {
            output,
            stdout,
            log,
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Run {
    pub fn describe(&self) -> String {
        format!(
            "exit: {:?}\n--- host stdout ---\n{}\n--- host stderr ---\n{}\n--- loader log ---\n{}",
            self.output.status.code(),
            self.stdout,
            String::from_utf8_lossy(&self.output.stderr),
            self.log
        )
    }

    pub fn assert_success(&self) -> &Run {
        assert!(
            self.output.status.success(),
            "the host failed\n{}",
            self.describe()
        );
        self
    }

    pub fn assert_log_contains(&self, text: &str) -> &Run {
        assert!(
            self.log.contains(text),
            "log lacks {text:?}\n{}",
            self.describe()
        );
        self
    }

    pub fn assert_log_lacks(&self, text: &str) -> &Run {
        assert!(
            !self.log.contains(text),
            "log should not contain {text:?}\n{}",
            self.describe()
        );
        self
    }

    /// A `key=value` line the host printed.
    pub fn value(&self, key: &str) -> Option<String> {
        self.stdout
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_string))
    }
}

/// Version the loader announces (it follows the workspace version).
pub const LOADER_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---- PE headers ------------------------------------------------------------

fn u16_at(bytes: &[u8], offset: usize) -> usize {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as usize
}

fn u32_at(bytes: &[u8], offset: usize) -> usize {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize
}

/// Locates a 64-bit PE image's data directory `index`, and a function that
/// maps RVAs to file offsets.
fn pe_directory(bytes: &[u8], index: usize) -> (usize, impl Fn(usize) -> usize + '_) {
    assert_eq!(&bytes[..2], b"MZ");
    let nt = u32_at(bytes, 0x3C);
    assert_eq!(&bytes[nt..nt + 4], b"PE\0\0");
    let section_count = u16_at(bytes, nt + 6);
    let optional_size = u16_at(bytes, nt + 20);
    let optional = nt + 24;
    assert_eq!(u16_at(bytes, optional), 0x20B, "expected a PE32+ image");
    let rva = u32_at(bytes, optional + 112 + index * 8);
    let sections = optional + optional_size;
    let to_offset = move |rva: usize| -> usize {
        for section in 0..section_count {
            let header = sections + section * 40;
            let virtual_size = u32_at(bytes, header + 8);
            let virtual_address = u32_at(bytes, header + 12);
            let raw_size = u32_at(bytes, header + 16);
            let raw_pointer = u32_at(bytes, header + 20);
            if rva >= virtual_address && rva < virtual_address + virtual_size.max(raw_size) {
                return rva - virtual_address + raw_pointer;
            }
        }
        panic!("RVA 0x{rva:X} is in no section");
    };
    (rva, to_offset)
}

fn c_string_at(bytes: &[u8], offset: usize) -> String {
    let end = bytes[offset..].iter().position(|b| *b == 0).unwrap() + offset;
    String::from_utf8(bytes[offset..end].to_vec()).unwrap()
}

/// Names in the export table of a 64-bit PE image, in table order.
pub fn pe_export_names(bytes: &[u8]) -> Vec<String> {
    let (rva, to_offset) = pe_directory(bytes, 0);
    if rva == 0 {
        return Vec::new();
    }
    let directory = to_offset(rva);
    let count = u32_at(bytes, directory + 24);
    let names = to_offset(u32_at(bytes, directory + 32));
    (0..count)
        .map(|index| c_string_at(bytes, to_offset(u32_at(bytes, names + index * 4))))
        .collect()
}

/// DLLs named in the import table of a 64-bit PE image.
pub fn pe_imported_dlls(bytes: &[u8]) -> Vec<String> {
    let (rva, to_offset) = pe_directory(bytes, 1);
    if rva == 0 {
        return Vec::new();
    }
    let mut descriptor = to_offset(rva);
    let mut dlls = Vec::new();
    while u32_at(bytes, descriptor + 12) != 0 {
        dlls.push(c_string_at(
            bytes,
            to_offset(u32_at(bytes, descriptor + 12)),
        ));
        descriptor += 20;
    }
    dlls
}
