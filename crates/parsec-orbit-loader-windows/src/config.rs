//! Loader settings, all overridable through the process environment so a user
//! can change behaviour without reinstalling anything. Where the files live is
//! decided by `parsec-orbit-layout`; this module adds what only the Windows
//! loader has: the DevTools port, the host check and the debug console.

use std::path::{Path, PathBuf};

use parsec_orbit_layout::{Inputs, Layout, Os};

/// Default port the WebView2 devtools endpoint is opened on.
pub const DEFAULT_DEVTOOLS_PORT: u16 = 9222;

/// Process the plugin work runs in unless `PARSEC_ORBIT_HOST=any` overrides it.
pub const CLIENT_PROCESS: &str = "parsecd.exe";

pub struct Config {
    /// Log destinations in priority order; the first writable one wins.
    pub log_candidates: Vec<PathBuf>,
    pub plugin_dirs: Vec<PathBuf>,
    /// Parent of the per-plugin data directories; `None` without `%APPDATA%`.
    pub data_root: Option<PathBuf>,
    /// Where Orbit installs plugins and where it stages updates of loaded
    /// ones; `None` without `%APPDATA%`.
    pub staging: Option<(PathBuf, PathBuf)>,
    /// `None` disables the devtools arguments.
    pub devtools_port: Option<u16>,
    /// Run the full plugin work even when the host is not the client.
    pub any_host: bool,
    /// Which instances open a console window at start-up and mirror the log
    /// into it.
    pub console: parsec_orbit_core::console::Target,
    /// Parsec's own logs, in priority order, that the console follows
    /// alongside the loader log; empty without `%APPDATA%`.
    pub parsec_logs: Vec<PathBuf>,
}

impl Config {
    /// `loader_dir` is the directory holding `bcrypt.dll`, i.e. the client
    /// application directory.
    pub fn load(loader_dir: Option<&Path>) -> Self {
        Self::from_lookup(loader_dir, |name| std::env::var(name).ok())
    }

    /// Same as [`Config::load`], reading variables through `lookup` instead of
    /// the process environment. An empty value counts as unset.
    pub fn from_lookup(loader_dir: Option<&Path>, lookup: impl Fn(&str) -> Option<String>) -> Self {
        let env = |name: &str| lookup(name).filter(|value| !value.is_empty());
        let layout = Layout::resolve(
            Os::Windows,
            Inputs {
                loader_dir,
                home: None,
            },
            &lookup,
        );

        let devtools_port = match env("PARSEC_ORBIT_DEVTOOLS").as_deref() {
            Some("0") | Some("off") | Some("false") => None,
            _ => Some(
                env("PARSEC_ORBIT_DEVTOOLS_PORT")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(DEFAULT_DEVTOOLS_PORT),
            ),
        };

        // Parsec writes its own log into its per-user directory: `log_cl.txt`
        // for the client the user started, `log.txt` for the instance the
        // service starts for hosting. The console shows both next to the
        // loader's lines, which is what makes the window worth having.
        let mut parsec_logs = Vec::new();
        if let Some(appdata) = env("APPDATA") {
            let dir = Path::new(&appdata).join("Parsec");
            parsec_logs.push(dir.join("log_cl.txt"));
            parsec_logs.push(dir.join("log.txt"));
        }

        Config {
            staging: layout.managed_plugins_dir().zip(layout.pending_dir()),
            log_candidates: layout.log_candidates,
            plugin_dirs: layout.plugin_dirs,
            data_root: layout.data_root,
            devtools_port,
            any_host: matches!(env("PARSEC_ORBIT_HOST").as_deref(), Some("any")),
            console: parsec_orbit_core::console::target(env("PARSEC_ORBIT_CONSOLE").as_deref()),
            parsec_logs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_core::console::Target;
    use std::collections::HashMap;

    fn load(dir: Option<&str>, vars: &[(&str, &str)]) -> Config {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        Config::from_lookup(dir.map(Path::new), |name| vars.get(name).cloned())
    }

    fn paths(list: &[PathBuf]) -> Vec<String> {
        list.iter()
            .map(|path| path.to_string_lossy().replace('/', "\\"))
            .collect()
    }

    #[test]
    fn defaults_without_any_environment() {
        let config = load(None, &[]);
        assert!(config.log_candidates.is_empty());
        assert!(config.plugin_dirs.is_empty());
        assert_eq!(config.devtools_port, Some(DEFAULT_DEVTOOLS_PORT));
        assert!(!config.any_host);
        assert_eq!(config.console, Target::Off);
        assert!(config.parsec_logs.is_empty());
    }

    #[test]
    fn empty_values_count_as_unset() {
        let config = load(
            Some("C:\\Parsec"),
            &[
                ("PARSEC_ORBIT_LOG", ""),
                ("PARSEC_ORBIT_PLUGIN_DIR", ""),
                ("APPDATA", ""),
                ("TEMP", ""),
                ("PARSEC_ORBIT_DEVTOOLS_PORT", ""),
                ("PARSEC_ORBIT_HOST", ""),
                ("PARSEC_ORBIT_CONSOLE", ""),
            ],
        );
        // The paths themselves are `parsec-orbit-layout`'s business.
        assert_eq!(config.log_candidates.len(), 1);
        assert_eq!(config.plugin_dirs.len(), 1);
        assert_eq!(config.devtools_port, Some(DEFAULT_DEVTOOLS_PORT));
        assert!(!config.any_host);
        assert_eq!(config.console, Target::Off);
    }

    #[test]
    fn devtools_can_be_switched_off() {
        for off in ["0", "off", "false"] {
            let config = load(None, &[("PARSEC_ORBIT_DEVTOOLS", off)]);
            assert_eq!(config.devtools_port, None, "PARSEC_ORBIT_DEVTOOLS={off}");
        }
    }

    #[test]
    fn devtools_other_values_keep_it_on() {
        for on in ["1", "on", "true", "OFF", "anything"] {
            let config = load(None, &[("PARSEC_ORBIT_DEVTOOLS", on)]);
            assert_eq!(
                config.devtools_port,
                Some(DEFAULT_DEVTOOLS_PORT),
                "PARSEC_ORBIT_DEVTOOLS={on}"
            );
        }
    }

    #[test]
    fn devtools_port_is_parsed_and_falls_back_to_the_default() {
        let port = |value: &str| load(None, &[("PARSEC_ORBIT_DEVTOOLS_PORT", value)]).devtools_port;
        assert_eq!(port("9333"), Some(9333));
        assert_eq!(port("1"), Some(1));
        assert_eq!(port("65535"), Some(65535));
        // Not a port: ignored rather than disabling the endpoint.
        assert_eq!(port("65536"), Some(DEFAULT_DEVTOOLS_PORT));
        assert_eq!(port("-1"), Some(DEFAULT_DEVTOOLS_PORT));
        assert_eq!(port("abc"), Some(DEFAULT_DEVTOOLS_PORT));
        assert_eq!(port(" 9333"), Some(DEFAULT_DEVTOOLS_PORT));
    }

    #[test]
    fn switching_devtools_off_wins_over_a_port() {
        let config = load(
            None,
            &[
                ("PARSEC_ORBIT_DEVTOOLS", "off"),
                ("PARSEC_ORBIT_DEVTOOLS_PORT", "9333"),
            ],
        );
        assert_eq!(config.devtools_port, None);
    }

    #[test]
    fn any_host_only_for_the_exact_value() {
        assert!(load(None, &[("PARSEC_ORBIT_HOST", "any")]).any_host);
        for other in ["ANY", "all", "1", "parsecd.exe"] {
            assert!(
                !load(None, &[("PARSEC_ORBIT_HOST", other)]).any_host,
                "PARSEC_ORBIT_HOST={other}"
            );
        }
    }

    #[test]
    fn the_console_setting_picks_the_instances() {
        let console = |value: &str| load(None, &[("PARSEC_ORBIT_CONSOLE", value)]).console;
        for on in ["1", "on", "true", "yes"] {
            assert_eq!(console(on), Target::All, "PARSEC_ORBIT_CONSOLE={on}");
        }
        assert_eq!(console("client"), Target::Client);
        assert_eq!(console("host"), Target::Host);
        for off in ["", "0", "off", "false", "ON", "1 ", "anything"] {
            assert_eq!(console(off), Target::Off, "PARSEC_ORBIT_CONSOLE={off}");
        }
    }

    #[test]
    fn parsec_logs_are_the_client_and_host_logs_below_appdata() {
        let config = load(None, &[("APPDATA", "C:\\Roaming")]);
        assert_eq!(
            paths(&config.parsec_logs),
            [
                "C:\\Roaming\\Parsec\\log_cl.txt",
                "C:\\Roaming\\Parsec\\log.txt",
            ],
            "the client's log first, then the one the service writes"
        );

        // A service environment has no %APPDATA% and nothing to follow.
        assert!(load(None, &[]).parsec_logs.is_empty());
    }

    #[test]
    fn client_process_name_is_the_parsec_executable() {
        assert_eq!(CLIENT_PROCESS, "parsecd.exe");
    }
}
