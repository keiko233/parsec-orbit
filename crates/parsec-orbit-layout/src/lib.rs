//! Where Parsec Orbit keeps its files.
//!
//! The loaders, the installer and the market plugin used to work the same
//! paths out five times (and the scripts a sixth). [`Layout::resolve`] is the
//! one place: a pure function of the OS, two directories the caller knows and
//! an injected environment lookup, so it is tested with plain values and never
//! reads the real environment itself.
//!
//! | | Windows | macOS |
//! | --- | --- | --- |
//! | Orbit root | `%APPDATA%\ParsecOrbit` | `PARSEC_ORBIT_HOME`, else `<home>/Library/Application Support/ParsecOrbit` |
//! | plugin directories | `PARSEC_ORBIT_PLUGIN_DIR` (`;`), `<client>\plugins`, `<root>\plugins` | `PARSEC_ORBIT_PLUGIN_DIR` (`:`), `<root>/plugins` |
//! | log candidates | `PARSEC_ORBIT_LOG`, `<root>`, `<client>`, `%TEMP%` | `PARSEC_ORBIT_LOG`, `<root>`, `$TMPDIR` |
//! | data | `<root>\data` | `<root>/data` |
//!
//! Every variable counts as unset when it is empty. The specification is
//! `docs/spec/loading.md` and `docs/spec/macos-platform.md`.

use std::path::{Path, PathBuf};

/// Directory names inside the Orbit root.
pub const PLUGINS_DIR: &str = "plugins";
pub const DATA_DIR: &str = "data";
/// Updates staged for the next start ([`Layout::pending_dir`]).
pub const PENDING_DIR: &str = "pending";
/// The loader log file name, in every location it is offered.
pub const LOG_FILE: &str = "parsec-orbit.log";

/// An operating system Orbit runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
}

impl Os {
    /// The OS this build runs on; `None` where Orbit has no support.
    pub const fn current() -> Option<Os> {
        if cfg!(windows) {
            Some(Os::Windows)
        } else if cfg!(target_os = "macos") {
            Some(Os::MacOs)
        } else {
            None
        }
    }

    /// Separator of the `PARSEC_ORBIT_PLUGIN_DIR` list.
    pub const fn path_list_separator(self) -> char {
        match self {
            Os::Windows => ';',
            Os::MacOs => ':',
        }
    }
}

/// What the caller knows that the environment does not say.
#[derive(Clone, Copy, Default)]
pub struct Inputs<'a> {
    /// Windows: the directory holding the loader, i.e. the client directory.
    pub loader_dir: Option<&'a Path>,
    /// macOS: the real home of the current user.
    pub home: Option<&'a Path>,
}

/// The resolved locations. Lists are in priority order, duplicates dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The Orbit root; `None` on Windows without `%APPDATA%` (a service
    /// environment).
    pub root: Option<PathBuf>,
    pub plugin_dirs: Vec<PathBuf>,
    /// Log destinations; the first writable one wins.
    pub log_candidates: Vec<PathBuf>,
    /// Parent of the per-plugin data directories.
    pub data_root: Option<PathBuf>,
}

fn split_paths(value: &str, separator: char) -> Vec<PathBuf> {
    value
        .split(separator)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Drops duplicates; the first spelling wins. Windows paths compare ASCII
/// case-insensitively, `/` and `\` alike; macOS paths compare by canonical path (spellings of a
/// directory that does not exist yet compare as written).
fn dedupe(os: Os, paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut unique: Vec<(String, PathBuf)> = Vec::with_capacity(paths.len());
    for path in paths {
        let key = match os {
            // `/` and `\` name the same directory on Windows; comparing them
            // alike also keeps the result independent of the host's `join`.
            Os::Windows => path
                .to_string_lossy()
                .replace('/', "\\")
                .to_ascii_lowercase(),
            Os::MacOs => std::fs::canonicalize(&path)
                .unwrap_or_else(|_| path.clone())
                .to_string_lossy()
                .into_owned(),
        };
        if unique.iter().any(|(existing, _)| *existing == key) {
            continue;
        }
        unique.push((key, path));
    }
    unique.into_iter().map(|(_, path)| path).collect()
}

impl Layout {
    /// The plugin directory Orbit itself installs into (`<root>/plugins`).
    pub fn managed_plugins_dir(&self) -> Option<PathBuf> {
        self.root.as_ref().map(|root| root.join(PLUGINS_DIR))
    }

    /// Where updates of loaded plugins are staged (`<root>/pending`); the
    /// loader promotes them before it maps anything.
    pub fn pending_dir(&self) -> Option<PathBuf> {
        self.root.as_ref().map(|root| root.join(PENDING_DIR))
    }

    /// A layout below a fixed Orbit root, ignoring the environment. The macOS
    /// activated host uses it: its paths come from the installation record,
    /// not from variables a process could set.
    pub fn for_root(root: PathBuf) -> Layout {
        Layout {
            plugin_dirs: vec![root.join(PLUGINS_DIR)],
            log_candidates: vec![root.join(LOG_FILE)],
            data_root: Some(root.join(DATA_DIR)),
            root: Some(root),
        }
    }

    /// Works the locations out of `lookup` (an empty value counts as unset).
    pub fn resolve(os: Os, inputs: Inputs<'_>, lookup: impl Fn(&str) -> Option<String>) -> Layout {
        let env = |name: &str| lookup(name).filter(|value| !value.is_empty());

        let root = match os {
            Os::Windows => env("APPDATA").map(|appdata| Path::new(&appdata).join("ParsecOrbit")),
            Os::MacOs => Some(
                env("PARSEC_ORBIT_HOME")
                    .map(PathBuf::from)
                    .or_else(|| {
                        inputs.home.map(|home| {
                            home.join("Library")
                                .join("Application Support")
                                .join("ParsecOrbit")
                        })
                    })
                    .unwrap_or_else(|| PathBuf::from("ParsecOrbit")),
            ),
        };

        let mut plugin_dirs = Vec::new();
        if let Some(extra) = env("PARSEC_ORBIT_PLUGIN_DIR") {
            plugin_dirs.extend(split_paths(&extra, os.path_list_separator()));
        }
        if os == Os::Windows {
            if let Some(dir) = inputs.loader_dir {
                plugin_dirs.push(dir.join(PLUGINS_DIR));
            }
        }
        if let Some(root) = &root {
            plugin_dirs.push(root.join(PLUGINS_DIR));
        }

        // Every candidate is probed at start-up, so a path that cannot be
        // written (a missing %APPDATA%, an unelevated client directory, a
        // read-only volume, a file someone else holds) costs the next one, not
        // the whole log.
        let mut log_candidates = Vec::new();
        if let Some(path) = env("PARSEC_ORBIT_LOG") {
            log_candidates.push(PathBuf::from(path));
        }
        if let Some(root) = &root {
            log_candidates.push(root.join(LOG_FILE));
        }
        match os {
            Os::Windows => {
                if let Some(dir) = inputs.loader_dir {
                    log_candidates.push(dir.join(LOG_FILE));
                }
                if let Some(temp) = env("TEMP") {
                    log_candidates.push(Path::new(&temp).join(LOG_FILE));
                }
            }
            Os::MacOs => {
                if let Some(temp) = env("TMPDIR") {
                    log_candidates.push(Path::new(&temp).join(LOG_FILE));
                }
            }
        }

        Layout {
            data_root: root.as_ref().map(|root| root.join(DATA_DIR)),
            plugin_dirs: dedupe(os, plugin_dirs),
            log_candidates: dedupe(os, log_candidates),
            root,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn resolve(os: Os, inputs: Inputs<'_>, vars: &[(&str, &str)]) -> Layout {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        Layout::resolve(os, inputs, |name| vars.get(name).cloned())
    }

    fn windows(dir: Option<&str>, vars: &[(&str, &str)]) -> Layout {
        resolve(
            Os::Windows,
            Inputs {
                loader_dir: dir.map(Path::new),
                home: None,
            },
            vars,
        )
    }

    fn macos(home: Option<&str>, vars: &[(&str, &str)]) -> Layout {
        resolve(
            Os::MacOs,
            Inputs {
                loader_dir: None,
                home: home.map(Path::new),
            },
            vars,
        )
    }

    /// Windows expectations are written with backslashes; `Path::join` on
    /// another host uses `/`, so both spellings are normalised.
    fn win(list: &[PathBuf]) -> Vec<String> {
        list.iter()
            .map(|path| path.to_string_lossy().replace('/', "\\"))
            .collect()
    }

    fn mac(list: &[PathBuf]) -> Vec<String> {
        list.iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    // ---- shared -----------------------------------------------------------

    #[test]
    fn split_paths_trims_and_drops_empty_parts() {
        assert_eq!(
            split_paths(" C:\\a ;; D:\\b;  ;E:\\c;", ';'),
            vec![
                PathBuf::from("C:\\a"),
                PathBuf::from("D:\\b"),
                PathBuf::from("E:\\c")
            ]
        );
        assert_eq!(
            split_paths(" /a :/b : /c ", ':'),
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c")
            ]
        );
        assert!(split_paths("", ';').is_empty());
        assert!(split_paths(" ; ;", ';').is_empty());
        assert!(split_paths(" : : ", ':').is_empty());
    }

    #[test]
    fn for_root_ignores_the_environment() {
        let layout = Layout::for_root(PathBuf::from("/orbit"));
        assert_eq!(mac(&layout.plugin_dirs), ["/orbit/plugins"]);
        assert_eq!(mac(&layout.log_candidates), ["/orbit/parsec-orbit.log"]);
        assert_eq!(layout.data_root.as_deref(), Some(Path::new("/orbit/data")));
        assert_eq!(layout.root.as_deref(), Some(Path::new("/orbit")));
    }

    #[test]
    fn staged_updates_live_next_to_the_managed_plugins() {
        let layout = Layout::for_root(PathBuf::from("/orbit"));
        assert_eq!(
            layout.managed_plugins_dir(),
            Some(PathBuf::from("/orbit/plugins"))
        );
        assert_eq!(layout.pending_dir(), Some(PathBuf::from("/orbit/pending")));
        let service = windows(Some("C:\\Parsec"), &[]);
        assert_eq!(service.pending_dir(), None, "no %APPDATA%, nothing staged");
    }

    #[test]
    fn the_current_os_is_known_here() {
        assert!(Os::current().is_some());
    }

    // ---- Windows ------------------------------------------------------------

    #[test]
    fn windows_without_any_environment_offers_nothing() {
        let layout = windows(None, &[]);
        assert!(layout.root.is_none());
        assert!(layout.log_candidates.is_empty());
        assert!(layout.plugin_dirs.is_empty());
        assert!(layout.data_root.is_none());
    }

    #[test]
    fn windows_plugin_dirs_are_ordered_override_then_client_then_appdata() {
        let layout = windows(
            Some("C:\\Parsec"),
            &[
                ("PARSEC_ORBIT_PLUGIN_DIR", "D:\\one;D:\\two"),
                ("APPDATA", "C:\\Users\\me\\AppData\\Roaming"),
            ],
        );
        assert_eq!(
            win(&layout.plugin_dirs),
            [
                "D:\\one",
                "D:\\two",
                "C:\\Parsec\\plugins",
                "C:\\Users\\me\\AppData\\Roaming\\ParsecOrbit\\plugins",
            ]
        );
    }

    #[test]
    fn windows_plugin_dirs_drop_duplicates_ignoring_case() {
        let layout = windows(
            Some("C:\\Parsec"),
            &[
                (
                    "PARSEC_ORBIT_PLUGIN_DIR",
                    "c:\\PARSEC\\plugins;D:\\other;d:\\OTHER",
                ),
                ("APPDATA", "C:\\Roaming"),
            ],
        );
        assert_eq!(
            win(&layout.plugin_dirs),
            [
                "c:\\PARSEC\\plugins",
                "D:\\other",
                "C:\\Roaming\\ParsecOrbit\\plugins",
            ]
        );
    }

    #[test]
    fn windows_plugin_data_lives_below_appdata() {
        let layout = windows(None, &[("APPDATA", "C:\\Roaming")]);
        assert_eq!(
            layout
                .data_root
                .map(|path| path.to_string_lossy().replace('/', "\\")),
            Some("C:\\Roaming\\ParsecOrbit\\data".to_string())
        );
        assert_eq!(windows(None, &[]).data_root, None);
        assert_eq!(windows(None, &[("APPDATA", "")]).data_root, None);
    }

    #[test]
    fn windows_plugin_dirs_skip_what_is_not_available() {
        let layout = windows(None, &[("APPDATA", "C:\\Roaming")]);
        assert_eq!(
            win(&layout.plugin_dirs),
            ["C:\\Roaming\\ParsecOrbit\\plugins"]
        );

        let layout = windows(Some("C:\\Parsec"), &[]);
        assert_eq!(win(&layout.plugin_dirs), ["C:\\Parsec\\plugins"]);
    }

    #[test]
    fn windows_log_candidates_are_ordered_by_priority() {
        let layout = windows(
            Some("C:\\Parsec"),
            &[
                ("PARSEC_ORBIT_LOG", "D:\\custom.log"),
                ("APPDATA", "C:\\Roaming"),
                ("TEMP", "C:\\Temp"),
            ],
        );
        assert_eq!(
            win(&layout.log_candidates),
            [
                "D:\\custom.log",
                "C:\\Roaming\\ParsecOrbit\\parsec-orbit.log",
                "C:\\Parsec\\parsec-orbit.log",
                "C:\\Temp\\parsec-orbit.log",
            ]
        );
    }

    #[test]
    fn windows_log_candidates_survive_a_missing_appdata() {
        // Service environments have no %APPDATA%; the other locations still
        // have to be offered.
        let layout = windows(Some("C:\\Parsec"), &[("TEMP", "C:\\Temp")]);
        assert_eq!(
            win(&layout.log_candidates),
            ["C:\\Parsec\\parsec-orbit.log", "C:\\Temp\\parsec-orbit.log"]
        );
    }

    #[test]
    fn windows_log_candidates_drop_duplicates_ignoring_case() {
        let layout = windows(
            Some("C:\\Parsec"),
            &[
                ("PARSEC_ORBIT_LOG", "c:\\PARSEC\\Parsec-Orbit.log"),
                ("TEMP", "C:\\Temp"),
            ],
        );
        assert_eq!(
            win(&layout.log_candidates),
            ["c:\\PARSEC\\Parsec-Orbit.log", "C:\\Temp\\parsec-orbit.log"],
            "the first spelling wins and the later duplicate is dropped"
        );
    }

    #[test]
    fn windows_empty_values_count_as_unset() {
        let layout = windows(
            Some("C:\\Parsec"),
            &[
                ("PARSEC_ORBIT_LOG", ""),
                ("PARSEC_ORBIT_PLUGIN_DIR", ""),
                ("APPDATA", ""),
                ("TEMP", ""),
            ],
        );
        assert_eq!(
            win(&layout.log_candidates),
            ["C:\\Parsec\\parsec-orbit.log"]
        );
        assert_eq!(win(&layout.plugin_dirs), ["C:\\Parsec\\plugins"]);
    }

    // ---- macOS --------------------------------------------------------------

    const MAC_ROOT: &str = "/Users/me/Library/Application Support/ParsecOrbit";

    #[test]
    fn the_orbit_root_defaults_below_the_real_home() {
        let layout = macos(Some("/Users/me"), &[]);
        assert_eq!(layout.root, Some(PathBuf::from(MAC_ROOT)));
        assert_eq!(mac(&layout.plugin_dirs), [format!("{MAC_ROOT}/plugins")]);
        assert_eq!(
            mac(&layout.log_candidates),
            [format!("{MAC_ROOT}/parsec-orbit.log")]
        );
        assert_eq!(
            layout.data_root.as_deref(),
            Some(Path::new(&format!("{MAC_ROOT}/data")))
        );
    }

    #[test]
    fn parsec_orbit_home_overrides_the_orbit_root() {
        let layout = macos(
            Some("/Users/me"),
            &[("PARSEC_ORBIT_HOME", "/private/tmp/orbit-test")],
        );
        assert_eq!(layout.root, Some(PathBuf::from("/private/tmp/orbit-test")));
        assert_eq!(
            mac(&layout.plugin_dirs),
            ["/private/tmp/orbit-test/plugins"]
        );
    }

    #[test]
    fn macos_plugin_dirs_are_override_then_orbit_root() {
        let layout = macos(
            Some("/Users/me"),
            &[("PARSEC_ORBIT_PLUGIN_DIR", "/one:/two")],
        );
        assert_eq!(
            mac(&layout.plugin_dirs),
            [
                "/one".to_string(),
                "/two".to_string(),
                format!("{MAC_ROOT}/plugins")
            ]
        );
    }

    #[test]
    fn macos_plugin_dirs_drop_duplicates_by_canonical_path() {
        let dir = std::env::temp_dir().join(format!("parsec-orbit-layout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.to_string_lossy().into_owned();
        let layout = macos(
            Some("/Users/me"),
            &[(
                "PARSEC_ORBIT_PLUGIN_DIR",
                // The second spelling resolves to the same directory.
                &format!("{real}:{real}/."),
            )],
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            mac(&layout.plugin_dirs),
            [real, format!("{MAC_ROOT}/plugins")],
            "the canonical duplicate is dropped, the first spelling wins"
        );
    }

    #[test]
    fn macos_log_candidates_are_log_then_orbit_root_then_tmpdir() {
        let layout = macos(
            Some("/Users/me"),
            &[("PARSEC_ORBIT_LOG", "/custom.log"), ("TMPDIR", "/tmp/me/")],
        );
        assert_eq!(
            mac(&layout.log_candidates),
            [
                "/custom.log".to_string(),
                format!("{MAC_ROOT}/parsec-orbit.log"),
                "/tmp/me/parsec-orbit.log".to_string(),
            ]
        );
    }

    #[test]
    fn macos_without_a_home_still_yields_env_driven_paths() {
        let layout = macos(None, &[("PARSEC_ORBIT_HOME", "/private/tmp/x")]);
        assert_eq!(layout.root, Some(PathBuf::from("/private/tmp/x")));
        assert_eq!(mac(&layout.plugin_dirs), ["/private/tmp/x/plugins"]);
        // Nothing at all: a relative root, never a panic.
        assert_eq!(macos(None, &[]).root, Some(PathBuf::from("ParsecOrbit")));
    }

    #[test]
    fn macos_empty_values_count_as_unset() {
        let layout = macos(
            Some("/Users/me"),
            &[
                ("PARSEC_ORBIT_HOME", ""),
                ("PARSEC_ORBIT_PLUGIN_DIR", ""),
                ("PARSEC_ORBIT_LOG", ""),
                ("TMPDIR", ""),
            ],
        );
        assert_eq!(layout.root, Some(PathBuf::from(MAC_ROOT)));
        assert_eq!(layout.log_candidates.len(), 1);
    }
}
