//! Loader settings for macOS, all overridable through the process
//! environment so a user can change behaviour without reinstalling anything.
//!
//! Where the files live is decided by `parsec-orbit-layout`. Paths follow
//! `docs/spec/macos-platform.md`: the Orbit root is
//! `<real home>/Library/Application Support/ParsecOrbit`, overridden by
//! `PARSEC_ORBIT_HOME` (used by tests). There is no client-directory plugin
//! directory: the patched app must not carry user data.

use std::path::{Path, PathBuf};

use parsec_orbit_layout::{Inputs, Layout, Os};

pub struct Config {
    /// The resolved Orbit root (`~/Library/Application Support/ParsecOrbit`).
    pub orbit_root: PathBuf,
    /// Log destinations in priority order; the first writable one wins.
    pub log_candidates: Vec<PathBuf>,
    /// Plugin directories in priority order, duplicates dropped.
    pub plugin_dirs: Vec<PathBuf>,
    /// Parent of the per-plugin data directories.
    pub data_root: Option<PathBuf>,
    /// Where Orbit installs plugins and where it stages updates of loaded
    /// ones.
    pub staging: Option<(PathBuf, PathBuf)>,
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

impl Config {
    /// Settings derived from a validated installation record.
    ///
    /// The activated vendor host must not take its paths from the process
    /// environment: `orbit_root` comes from `orbit-install.json`, and the
    /// plugin/data/log paths follow it. The loader-as-host development route
    /// keeps the environment overrides in [`Config::load`].
    pub fn for_root(orbit_root: PathBuf) -> Self {
        Self::from_layout(Layout::for_root(orbit_root))
    }

    /// Reads the settings from the process environment.
    pub fn load() -> Self {
        let home = real_home();
        Self::from_lookup(home.as_deref(), |name| std::env::var(name).ok())
    }

    /// Same as [`Config::load`], with the home and the variables supplied by
    /// the caller. An empty value counts as unset.
    pub fn from_lookup(home: Option<&Path>, lookup: impl Fn(&str) -> Option<String>) -> Self {
        Self::from_layout(Layout::resolve(
            Os::MacOs,
            Inputs {
                loader_dir: None,
                home,
            },
            lookup,
        ))
    }

    fn from_layout(layout: Layout) -> Self {
        Config {
            staging: layout.managed_plugins_dir().zip(layout.pending_dir()),
            orbit_root: layout.root.unwrap_or_else(|| PathBuf::from("ParsecOrbit")),
            log_candidates: layout.log_candidates,
            plugin_dirs: layout.plugin_dirs,
            data_root: layout.data_root,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn load(home: Option<&str>, vars: &[(&str, &str)]) -> Config {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        Config::from_lookup(home.map(Path::new), |name| vars.get(name).cloned())
    }

    // The path rules themselves are tested in `parsec-orbit-layout`; these
    // check that the macOS loader feeds it the real home and the environment.

    #[test]
    fn the_settings_follow_the_layout() {
        let config = load(Some("/Users/me"), &[("PARSEC_ORBIT_LOG", "/custom.log")]);
        let root = PathBuf::from("/Users/me/Library/Application Support/ParsecOrbit");
        assert_eq!(config.orbit_root, root);
        assert_eq!(config.plugin_dirs, [root.join("plugins")]);
        assert_eq!(
            config.log_candidates,
            [PathBuf::from("/custom.log"), root.join("parsec-orbit.log")]
        );
        assert_eq!(config.data_root, Some(root.join("data")));
    }

    #[test]
    fn an_installation_record_root_ignores_the_environment() {
        let config = Config::for_root(PathBuf::from("/private/tmp/orbit"));
        assert_eq!(config.orbit_root, PathBuf::from("/private/tmp/orbit"));
        assert_eq!(
            config.log_candidates,
            [PathBuf::from("/private/tmp/orbit/parsec-orbit.log")]
        );
    }
}
