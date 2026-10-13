//! Loader settings for macOS, all overridable through the process
//! environment so a user can change behaviour without reinstalling anything.
//!
//! Paths follow `docs/spec/macos-platform.md`: the Orbit root is
//! `<real home>/Library/Application Support/ParsecOrbit`, overridden by
//! `PARSEC_ORBIT_HOME` (used by tests). There is no client-directory plugin
//! directory: the patched app must not carry user data.

use std::path::{Path, PathBuf};

/// Directory names inside the Orbit root.
pub const PLUGINS_DIR: &str = "plugins";
pub const DATA_DIR: &str = "data";
pub const LOG_FILE: &str = "parsec-orbit.log";

pub struct Config {
    /// The resolved Orbit root (`~/Library/Application Support/ParsecOrbit`).
    pub orbit_root: PathBuf,
    /// Log destinations in priority order; the first writable one wins.
    pub log_candidates: Vec<PathBuf>,
    /// Plugin directories in priority order, duplicates dropped.
    pub plugin_dirs: Vec<PathBuf>,
    /// Parent of the per-plugin data directories.
    pub data_root: Option<PathBuf>,
}

/// The real home of the current user, straight from the passwd database.
///
/// The loader's own bindings are not interposed (dyld interposition applies
/// to the other images in the process), so this returns the real home even
/// after the client starts seeing the isolated one.
pub fn real_home() -> Option<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0u8; 4096];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    loop {
        // Safety: `entry`, `result` and the buffer are valid for the call.
        let code = unsafe {
            libc::getpwuid_r(
                uid,
                &mut entry,
                buffer.as_mut_ptr() as *mut libc::c_char,
                buffer.len(),
                &mut result,
            )
        };
        if code == libc::ERANGE && buffer.len() < 1 << 20 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if code != 0 || result.is_null() || entry.pw_dir.is_null() {
            return None;
        }
        // Safety: `pw_dir` is a valid C string owned by the buffer.
        let home = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
        return Some(PathBuf::from(
            String::from_utf8_lossy(home.to_bytes()).into_owned(),
        ));
    }
}

fn split_paths(value: &str) -> Vec<PathBuf> {
    value
        .split(':')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Drops duplicates by canonicalised path, case-sensitively; the first
/// spelling wins. Paths that do not exist yet compare as they were written.
fn dedupe(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut unique: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if unique.iter().any(|(existing, _)| *existing == canonical) {
            continue;
        }
        unique.push((canonical, dir));
    }
    unique.into_iter().map(|(_, dir)| dir).collect()
}

impl Config {
    /// Settings derived from a validated installation record.
    ///
    /// The activated vendor host must not take its paths from the process
    /// environment: `orbit_root` comes from `orbit-install.json`, and the
    /// plugin/data/log paths follow it. The loader-as-host development route
    /// keeps the environment overrides in [`Config::load`].
    pub fn for_root(orbit_root: PathBuf) -> Self {
        Config {
            log_candidates: vec![orbit_root.join(LOG_FILE)],
            plugin_dirs: vec![orbit_root.join(PLUGINS_DIR)],
            data_root: Some(orbit_root.join(DATA_DIR)),
            orbit_root,
        }
    }

    /// Reads the settings from the process environment.
    pub fn load() -> Self {
        let home = real_home();
        Self::from_lookup(home.as_deref(), |name| std::env::var(name).ok())
    }

    /// Same as [`Config::load`], with the home and the variables supplied by
    /// the caller. An empty value counts as unset.
    pub fn from_lookup(home: Option<&Path>, lookup: impl Fn(&str) -> Option<String>) -> Self {
        let env = |name: &str| lookup(name).filter(|value| !value.is_empty());

        let orbit_root = env("PARSEC_ORBIT_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                home.map(|home| {
                    home.join("Library")
                        .join("Application Support")
                        .join("ParsecOrbit")
                })
            })
            .unwrap_or_else(|| PathBuf::from("ParsecOrbit"));

        let mut plugin_dirs = Vec::new();
        if let Some(extra) = env("PARSEC_ORBIT_PLUGIN_DIR") {
            plugin_dirs.extend(split_paths(&extra));
        }
        plugin_dirs.push(orbit_root.join(PLUGINS_DIR));
        let plugin_dirs = dedupe(plugin_dirs);

        // Every candidate is probed at start-up by `log::init`, so a path
        // that cannot be written costs the next one, not the whole log.
        let mut log_candidates = Vec::new();
        if let Some(path) = env("PARSEC_ORBIT_LOG") {
            log_candidates.push(PathBuf::from(path));
        }
        log_candidates.push(orbit_root.join(LOG_FILE));
        if let Some(temp) = env("TMPDIR") {
            log_candidates.push(Path::new(&temp).join(LOG_FILE));
        }
        let log_candidates = dedupe(log_candidates);

        Config {
            data_root: Some(orbit_root.join(DATA_DIR)),
            orbit_root,
            log_candidates,
            plugin_dirs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_core::testutil::TempDir;
    use std::collections::HashMap;

    fn load(home: Option<&str>, vars: &[(&str, &str)]) -> Config {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        Config::from_lookup(home.map(Path::new), |name| vars.get(name).cloned())
    }

    fn paths(list: &[PathBuf]) -> Vec<String> {
        list.iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn split_paths_is_colon_separated_and_trims() {
        assert_eq!(
            split_paths(" /a :/b : /c "),
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c")
            ]
        );
        assert_eq!(
            split_paths("/one:/two"),
            vec![PathBuf::from("/one"), PathBuf::from("/two")]
        );
        assert!(split_paths("").is_empty());
        assert!(split_paths(" : : ").is_empty());
    }

    #[test]
    fn the_orbit_root_defaults_below_the_real_home() {
        let config = load(Some("/Users/me"), &[]);
        assert_eq!(
            config.orbit_root,
            PathBuf::from("/Users/me/Library/Application Support/ParsecOrbit")
        );
        assert_eq!(
            paths(&config.plugin_dirs),
            ["/Users/me/Library/Application Support/ParsecOrbit/plugins"]
        );
        assert_eq!(
            paths(&config.log_candidates),
            ["/Users/me/Library/Application Support/ParsecOrbit/parsec-orbit.log"]
        );
        assert_eq!(
            config.data_root.as_deref(),
            Some(Path::new(
                "/Users/me/Library/Application Support/ParsecOrbit/data"
            ))
        );
    }

    #[test]
    fn parsec_orbit_home_overrides_the_orbit_root() {
        let config = load(
            Some("/Users/me"),
            &[("PARSEC_ORBIT_HOME", "/private/tmp/orbit-test")],
        );
        assert_eq!(config.orbit_root, PathBuf::from("/private/tmp/orbit-test"));
        assert_eq!(
            paths(&config.plugin_dirs),
            ["/private/tmp/orbit-test/plugins"]
        );
    }

    #[test]
    fn plugin_dirs_are_override_then_orbit_root() {
        let config = load(
            Some("/Users/me"),
            &[("PARSEC_ORBIT_PLUGIN_DIR", "/one:/two")],
        );
        assert_eq!(
            paths(&config.plugin_dirs),
            [
                "/one",
                "/two",
                "/Users/me/Library/Application Support/ParsecOrbit/plugins",
            ]
        );
    }

    #[test]
    fn plugin_dirs_drop_duplicates_by_canonical_path() {
        let dir = TempDir::new("config-dir");
        let real = dir.path().to_string_lossy().into_owned();
        let config = load(
            Some("/Users/me"),
            &[(
                "PARSEC_ORBIT_PLUGIN_DIR",
                // The second spelling resolves to the same directory.
                &format!("{real}:{real}/."),
            )],
        );
        assert_eq!(
            paths(&config.plugin_dirs),
            [
                real,
                "/Users/me/Library/Application Support/ParsecOrbit/plugins".to_string()
            ],
            "the canonical duplicate is dropped, the first spelling wins"
        );
    }

    #[test]
    fn log_candidates_are_log_then_orbit_root_then_tmpdir() {
        let config = load(
            Some("/Users/me"),
            &[("PARSEC_ORBIT_LOG", "/custom.log"), ("TMPDIR", "/tmp/me/")],
        );
        assert_eq!(
            paths(&config.log_candidates),
            [
                "/custom.log",
                "/Users/me/Library/Application Support/ParsecOrbit/parsec-orbit.log",
                "/tmp/me/parsec-orbit.log",
            ]
        );
    }

    #[test]
    fn an_empty_home_still_yields_env_driven_paths() {
        let config = load(None, &[("PARSEC_ORBIT_HOME", "/private/tmp/x")]);
        assert_eq!(config.orbit_root, PathBuf::from("/private/tmp/x"));
        assert_eq!(paths(&config.plugin_dirs), ["/private/tmp/x/plugins"]);
    }

    #[test]
    fn empty_values_count_as_unset() {
        let config = load(
            Some("/Users/me"),
            &[
                ("PARSEC_ORBIT_HOME", ""),
                ("PARSEC_ORBIT_PLUGIN_DIR", ""),
                ("PARSEC_ORBIT_LOG", ""),
                ("TMPDIR", ""),
            ],
        );
        assert_eq!(
            config.orbit_root,
            PathBuf::from("/Users/me/Library/Application Support/ParsecOrbit")
        );
        assert_eq!(config.log_candidates.len(), 1);
    }
}
