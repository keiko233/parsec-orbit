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
    pub native: PathBuf,
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
            .args(["-p", "parsec-orbit-plugin-high-bitrate-native"])
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
            native: debug.join("parsec_orbit_plugin_high_bitrate_native.dll"),
            callpaths: debug.join("callpaths.exe"),
            roundtrip: debug.join("roundtrip.exe"),
        };
        for path in [
            &artifacts.loader,
            &artifacts.hello,
            &artifacts.webview,
            &artifacts.native,
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
    }
}

#[derive(Clone, Copy)]
pub enum Plugin {
    Hello,
    Webview,
    Native,
}

impl Plugin {
    pub fn dll(self) -> &'static Path {
        match self {
            Plugin::Hello => &artifacts().hello,
            Plugin::Webview => &artifacts().webview,
            Plugin::Native => &artifacts().native,
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
