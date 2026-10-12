//! Loader settings, all overridable through the process environment so a user
//! can change behaviour without reinstalling anything.

use std::path::{Path, PathBuf};

/// Default port the WebView2 devtools endpoint is opened on.
pub const DEFAULT_DEVTOOLS_PORT: u16 = 9222;

/// Process the plugin work runs in unless `PARSEC_ORBIT_HOST=any` overrides it.
pub const CLIENT_PROCESS: &str = "parsecd.exe";

pub struct Config {
    pub log_path: PathBuf,
    pub plugin_dirs: Vec<PathBuf>,
    /// `None` disables the devtools arguments.
    pub devtools_port: Option<u16>,
    /// Run the full plugin work even when the host is not the client.
    pub any_host: bool,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn split_paths(value: &str) -> Vec<PathBuf> {
    value
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(PathBuf::from)
        .collect()
}

impl Config {
    /// `loader_dir` is the directory holding `bcrypt.dll`, i.e. the client
    /// application directory.
    pub fn load(loader_dir: Option<&Path>) -> Self {
        let mut plugin_dirs = Vec::new();
        if let Some(extra) = env("PARSEC_ORBIT_PLUGIN_DIR") {
            plugin_dirs.extend(split_paths(&extra));
        }
        if let Some(dir) = loader_dir {
            plugin_dirs.push(dir.join("plugins"));
        }
        if let Some(appdata) = env("APPDATA") {
            plugin_dirs.push(Path::new(&appdata).join("ParsecOrbit").join("plugins"));
        }

        let default_log = env("APPDATA")
            .map(|appdata| {
                Path::new(&appdata)
                    .join("ParsecOrbit")
                    .join("parsec-orbit.log")
            })
            .or_else(|| loader_dir.map(|dir| dir.join("parsec-orbit.log")))
            .unwrap_or_else(|| PathBuf::from("parsec-orbit.log"));

        let devtools_port = match env("PARSEC_ORBIT_DEVTOOLS").as_deref() {
            Some("0") | Some("off") | Some("false") => None,
            _ => Some(
                env("PARSEC_ORBIT_DEVTOOLS_PORT")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(DEFAULT_DEVTOOLS_PORT),
            ),
        };

        Config {
            log_path: env("PARSEC_ORBIT_LOG")
                .map(PathBuf::from)
                .unwrap_or(default_log),
            plugin_dirs,
            devtools_port,
            any_host: matches!(env("PARSEC_ORBIT_HOST").as_deref(), Some("any")),
        }
    }
}
