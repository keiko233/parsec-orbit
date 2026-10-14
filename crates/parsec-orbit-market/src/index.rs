//! The plugin index: what can be installed, where from, and how to check it.
//!
//! ```json
//! {
//!   "format": 1,
//!   "name": "Parsec Orbit",
//!   "plugins": [{
//!     "id": "com.parsec-orbit.hooks",
//!     "name": "Hooks",
//!     "description": "...",
//!     "author": "...",
//!     "license": "MIT",
//!     "versions": [{
//!       "version": "0.1.0",
//!       "api": 1,
//!       "url": "parsec-orbit-plugin-hooks.zip",
//!       "sha256": "<64 hex digits>",
//!       "size": 123456
//!     }]
//!   }]
//! }
//! ```
//!
//! `url` is relative to the index or absolute. The index is signed as a
//! whole (see [`crate::trust`]): its sha256 entries are what make a download
//! trustworthy, so the signature covers them.

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::Error;

/// The only `format` this version understands.
pub const FORMAT: u32 = 1;

/// Largest package accepted, in bytes.
pub const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;

/// The platform a release package targets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    #[serde(rename = "windows-x86_64")]
    #[default]
    WindowsX86_64,
    #[serde(rename = "macos-arm64")]
    MacosArm64,
}

impl Platform {
    /// Platform this build of the tool serves.
    pub const fn current() -> Self {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Self::MacosArm64
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            Self::WindowsX86_64
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WindowsX86_64 => "windows-x86_64",
            Self::MacosArm64 => "macos-arm64",
        }
    }

    fn is_default(platform: &Self) -> bool {
        *platform == Self::WindowsX86_64
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub format: u32,
    #[serde(default)]
    pub name: String,
    pub plugins: Vec<Plugin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plugin {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    pub versions: Vec<Release>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    /// Loader ABI version the release needs.
    #[serde(default = "default_api")]
    pub api: u32,
    /// Target platform. Missing values retain the original Windows meaning.
    #[serde(default, skip_serializing_if = "Platform::is_default")]
    pub platform: Platform,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// Ids the plugin needs installed first (its `requires`).
    #[serde(default)]
    pub requires: Vec<String>,
}

fn default_api() -> u32 {
    1
}

/// Whether `id` is acceptable as a plugin id and as a path component: 1 to 128
/// of `[A-Za-z0-9._-]`, not made of dots only.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !id.bytes().all(|byte| byte == b'.')
}

impl Index {
    /// Parses and checks an index.
    pub fn parse(bytes: &[u8]) -> Result<Index, Error> {
        let index: Index = serde_json::from_slice(bytes)
            .map_err(|error| Error::Index(format!("not a valid index: {error}")))?;
        index.validate()?;
        Ok(index)
    }

    fn validate(&self) -> Result<(), Error> {
        let bad = |message: String| Err(Error::Index(message));
        if self.format != FORMAT {
            return bad(format!(
                "index format {} is not supported (this tool reads {FORMAT})",
                self.format
            ));
        }
        let mut seen = Vec::new();
        for plugin in &self.plugins {
            if !valid_id(&plugin.id) {
                return bad(format!("invalid plugin id {:?}", plugin.id));
            }
            if seen.contains(&&plugin.id) {
                return bad(format!("plugin {} is listed twice", plugin.id));
            }
            seen.push(&plugin.id);
            if plugin.versions.is_empty() {
                return bad(format!("plugin {} has no versions", plugin.id));
            }
            let mut versions: Vec<(Version, Platform)> = Vec::new();
            for release in &plugin.versions {
                let version = Version::parse(&release.version).map_err(|error| {
                    Error::Index(format!(
                        "{} {:?} is not a semantic version: {error}",
                        plugin.id, release.version
                    ))
                })?;
                if versions.contains(&(version.clone(), release.platform)) {
                    return bad(format!(
                        "{} {version} for {} is listed twice",
                        plugin.id,
                        release.platform.as_str()
                    ));
                }
                versions.push((version, release.platform));
                if release.sha256.len() != 64
                    || !release
                        .sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return bad(format!(
                        "{} {}: sha256 must be 64 lowercase hex digits",
                        plugin.id, release.version
                    ));
                }
                if release.url.is_empty() {
                    return bad(format!("{} {}: no url", plugin.id, release.version));
                }
                if release.size == 0 || release.size > MAX_PACKAGE_BYTES {
                    return bad(format!(
                        "{} {}: size {} is outside 1..={MAX_PACKAGE_BYTES}",
                        plugin.id, release.version, release.size
                    ));
                }
                if let Some(id) = release.requires.iter().find(|id| !valid_id(id)) {
                    return bad(format!("{} requires the invalid id {id:?}", plugin.id));
                }
            }
        }
        Ok(())
    }

    pub fn plugin(&self, id: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|plugin| plugin.id == id)
    }

    /// Serialises with a stable layout (what gets signed).
    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("an index serialises");
        text.push('\n');
        text
    }
}

impl Plugin {
    /// The release to install: `version` if given, else the newest one that
    /// matches `platform`, needs no more than `api` and is not a pre-release.
    pub fn select(
        &self,
        version: Option<&str>,
        api: u32,
        platform: Platform,
    ) -> Result<&Release, Error> {
        if let Some(wanted) = version {
            let wanted = Version::parse(wanted)
                .map_err(|error| Error::Usage(format!("{wanted:?} is not a version: {error}")))?;
            let release = self
                .versions
                .iter()
                .find(|release| {
                    Version::parse(&release.version).ok().as_ref() == Some(&wanted)
                        && release.platform == platform
                })
                .or_else(|| {
                    self.versions.iter().find(|release| {
                        Version::parse(&release.version).ok().as_ref() == Some(&wanted)
                    })
                })
                .ok_or_else(|| Error::NotFound(format!("{} has no version {wanted}", self.id)))?;
            if release.platform != platform {
                return Err(Error::Incompatible(format!(
                    "{} {wanted} has no release for platform {}",
                    self.id,
                    platform.as_str()
                )));
            }
            if release.api > api {
                return Err(Error::Incompatible(format!(
                    "{} {wanted} needs loader API {} but this one implements {api}",
                    self.id, release.api
                )));
            }
            return Ok(release);
        }
        self.versions
            .iter()
            .filter(|release| release.platform == platform)
            .filter(|release| release.api <= api)
            .filter_map(|release| Some((Version::parse(&release.version).ok()?, release)))
            .filter(|(version, _)| version.pre.is_empty())
            .max_by(|(a, _), (b, _)| a.cmp(b))
            .map(|(_, release)| release)
            .ok_or_else(|| {
                Error::Incompatible(format!(
                    "{} has no release for platform {} and loader API {api}",
                    self.id,
                    platform.as_str()
                ))
            })
    }
}

impl Release {
    pub fn parsed_version(&self) -> Version {
        Version::parse(&self.version).expect("validated when the index was parsed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str, api: u32) -> Release {
        Release {
            version: version.to_string(),
            api,
            platform: Platform::WindowsX86_64,
            url: format!("p-{version}.zip"),
            sha256: "ab".repeat(32),
            size: 10,
            requires: Vec::new(),
        }
    }

    fn plugin(versions: Vec<Release>) -> Plugin {
        Plugin {
            id: "com.example.p".into(),
            name: String::new(),
            description: String::new(),
            author: String::new(),
            license: String::new(),
            versions,
        }
    }

    fn index_of(plugins: Vec<Plugin>) -> Vec<u8> {
        serde_json::to_vec(&Index {
            format: FORMAT,
            name: "test".into(),
            plugins,
        })
        .unwrap()
    }

    #[test]
    fn a_valid_index_round_trips() {
        let bytes = index_of(vec![plugin(vec![release("1.0.0", 1)])]);
        let index = Index::parse(&bytes).unwrap();
        assert_eq!(index.plugins.len(), 1);
        assert!(index.plugin("com.example.p").is_some());
        assert!(index.plugin("other").is_none());
        assert_eq!(Index::parse(index.to_json().as_bytes()).unwrap(), index);
    }

    #[test]
    fn defaults_fill_in_optional_fields() {
        let json = r#"{"format":1,"plugins":[{"id":"a","versions":[{"version":"1.0.0","url":"a.zip","sha256":"0000000000000000000000000000000000000000000000000000000000000000","size":1}]}]}"#;
        let index = Index::parse(json.as_bytes()).unwrap();
        assert_eq!(index.plugins[0].versions[0].api, 1);
        assert_eq!(
            index.plugins[0].versions[0].platform,
            Platform::WindowsX86_64
        );
        assert!(index.plugins[0].versions[0].requires.is_empty());
    }

    #[test]
    fn broken_indexes_are_refused_with_a_reason() {
        let check = |plugins: Vec<Plugin>, needle: &str| {
            let error = Index::parse(&index_of(plugins)).unwrap_err().to_string();
            assert!(error.contains(needle), "{error}");
        };
        let mut wrong_format =
            Index::parse(&index_of(vec![plugin(vec![release("1.0.0", 1)])])).unwrap();
        wrong_format.format = 2;
        assert!(Index::parse(&serde_json::to_vec(&wrong_format).unwrap())
            .unwrap_err()
            .to_string()
            .contains("not supported"));

        let mut bad_id = plugin(vec![release("1.0.0", 1)]);
        bad_id.id = "../evil".into();
        check(vec![bad_id], "invalid plugin id");
        check(
            vec![
                plugin(vec![release("1.0.0", 1)]),
                plugin(vec![release("1.0.0", 1)]),
            ],
            "listed twice",
        );
        check(vec![plugin(vec![])], "no versions");
        check(vec![plugin(vec![release("one", 1)])], "semantic version");
        check(
            vec![plugin(vec![release("1.0.0", 1), release("1.0.0", 1)])],
            "listed twice",
        );
        let mut short = release("1.0.0", 1);
        short.sha256 = "abc".into();
        check(vec![plugin(vec![short])], "sha256");
        let mut upper = release("1.0.0", 1);
        upper.sha256 = "AB".repeat(32);
        check(vec![plugin(vec![upper])], "lowercase");
        let mut empty_url = release("1.0.0", 1);
        empty_url.url = String::new();
        check(vec![plugin(vec![empty_url])], "no url");
        let mut huge = release("1.0.0", 1);
        huge.size = MAX_PACKAGE_BYTES + 1;
        check(vec![plugin(vec![huge])], "size");
        let mut needs = release("1.0.0", 1);
        needs.requires = vec!["a/b".into()];
        check(vec![plugin(vec![needs])], "invalid id");
        assert!(Index::parse(b"[]").is_err());
        assert!(Index::parse(b"").is_err());
    }

    #[test]
    fn ids_are_path_safe() {
        for good in ["a", "com.example.p", "A_b-c.1"] {
            assert!(valid_id(good), "{good}");
        }
        for bad in ["", "..", ".", "a/b", "a\\b", "a:b", "a b", &"x".repeat(129)] {
            assert!(!valid_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_newest_compatible_release_is_selected() {
        let p = plugin(vec![
            release("1.2.0", 1),
            release("1.10.0", 1),
            release("2.0.0", 2),
            release("1.11.0-beta.1", 1),
            release("1.9.0", 1),
        ]);
        assert_eq!(
            p.select(None, 1, Platform::WindowsX86_64).unwrap().version,
            "1.10.0",
            "semver, not text order"
        );
        assert_eq!(
            p.select(None, 2, Platform::WindowsX86_64).unwrap().version,
            "2.0.0"
        );
    }

    #[test]
    fn mixed_platform_versions_are_filtered_and_old_indexes_stay_windows() {
        let mut windows = release("2.0.0", 1);
        windows.platform = Platform::WindowsX86_64;
        let mut macos = release("1.5.0", 1);
        macos.platform = Platform::MacosArm64;
        let p = plugin(vec![windows, macos]);

        assert_eq!(
            p.select(None, 1, Platform::WindowsX86_64).unwrap().version,
            "2.0.0"
        );
        assert_eq!(
            p.select(None, 1, Platform::MacosArm64).unwrap().version,
            "1.5.0"
        );
        assert!(matches!(
            p.select(Some("2.0.0"), 1, Platform::MacosArm64),
            Err(Error::Incompatible(_))
        ));

        let old = Index::parse(
            br#"{"format":1,"plugins":[{"id":"legacy","versions":[{"version":"1.0.0","url":"p.zip","sha256":"0000000000000000000000000000000000000000000000000000000000000000","size":1}]}]}"#,
        )
        .unwrap();
        assert_eq!(old.plugins[0].versions[0].platform, Platform::WindowsX86_64);
        assert!(!old.to_json().contains("platform"));
    }

    #[test]
    fn one_semver_can_have_a_package_for_each_platform() {
        let mut windows = release("1.0.0", 1);
        windows.platform = Platform::WindowsX86_64;
        let mut macos = release("1.0.0", 1);
        macos.platform = Platform::MacosArm64;
        let parsed = Index::parse(&index_of(vec![plugin(vec![windows, macos])])).unwrap();
        let plugin = parsed.plugin("com.example.p").unwrap();

        assert_eq!(
            plugin
                .select(Some("1.0.0"), 1, Platform::WindowsX86_64)
                .unwrap()
                .platform,
            Platform::WindowsX86_64
        );
        assert_eq!(
            plugin
                .select(Some("1.0.0"), 1, Platform::MacosArm64)
                .unwrap()
                .platform,
            Platform::MacosArm64
        );
    }

    #[test]
    fn a_version_can_be_asked_for_and_must_be_compatible() {
        let p = plugin(vec![
            release("1.0.0", 1),
            release("2.0.0", 2),
            release("3.0.0-rc.1", 1),
        ]);
        assert_eq!(
            p.select(Some("1.0.0"), 1, Platform::WindowsX86_64)
                .unwrap()
                .version,
            "1.0.0"
        );
        assert_eq!(
            p.select(Some("3.0.0-rc.1"), 1, Platform::WindowsX86_64)
                .unwrap()
                .version,
            "3.0.0-rc.1"
        );
        assert!(matches!(
            p.select(Some("2.0.0"), 1, Platform::WindowsX86_64),
            Err(Error::Incompatible(_))
        ));
        assert!(matches!(
            p.select(Some("9.9.9"), 1, Platform::WindowsX86_64),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            p.select(Some("nope"), 1, Platform::WindowsX86_64),
            Err(Error::Usage(_))
        ));
        let only_new = plugin(vec![release("2.0.0", 2)]);
        assert!(matches!(
            only_new.select(None, 1, Platform::WindowsX86_64),
            Err(Error::Incompatible(_))
        ));
        let only_pre = plugin(vec![release("1.0.0-alpha", 1)]);
        assert!(
            only_pre.select(None, 1, Platform::WindowsX86_64).is_err(),
            "no pre-release by default"
        );
    }
}
