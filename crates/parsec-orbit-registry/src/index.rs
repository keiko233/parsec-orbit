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
//!       "url": "parsec-orbit-lib-hooks.zip",
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

/// The platform a release package targets, as `<os>-<arch>`: lowercase letters
/// and digits for the OS, lowercase letters, digits and `_` for the
/// architecture (`windows-x86_64`, `macos-arm64`). The set is open: an index may
/// name a platform this tool does not serve, and its releases are then never
/// offered here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PlatformId(String);

impl PlatformId {
    pub const WINDOWS_X86_64: &'static str = "windows-x86_64";
    pub const MACOS_ARM64: &'static str = "macos-arm64";

    pub fn windows_x86_64() -> Self {
        Self(Self::WINDOWS_X86_64.to_string())
    }

    pub fn macos_arm64() -> Self {
        Self(Self::MACOS_ARM64.to_string())
    }

    /// Platform this build of the tool serves; `None` on any other OS or
    /// architecture, which has no packages to install.
    pub fn current() -> Option<Self> {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some(Self::macos_arm64())
        } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Some(Self::windows_x86_64())
        } else {
            None
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Releases without a `platform` key are Windows releases.
    fn is_default(platform: &Self) -> bool {
        platform.0 == Self::WINDOWS_X86_64
    }
}

impl Default for PlatformId {
    fn default() -> Self {
        Self::windows_x86_64()
    }
}

impl TryFrom<String> for PlatformId {
    type Error = Error;

    fn try_from(text: String) -> Result<Self, Error> {
        if valid_platform(&text) {
            Ok(Self(text))
        } else {
            Err(Error::Index(format!(
                "platform {text:?} is not <os>-<arch> (lowercase letters and digits, then _ or digits in the architecture)"
            )))
        }
    }
}

impl From<PlatformId> for String {
    fn from(platform: PlatformId) -> String {
        platform.0
    }
}

/// `^[a-z0-9]+-[a-z0-9_]+$`, checked without a regular expression engine.
fn valid_platform(text: &str) -> bool {
    let Some((os, arch)) = text.split_once('-') else {
        return false;
    };
    !os.is_empty()
        && !arch.is_empty()
        && os
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && arch
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub format: u32,
    #[serde(default)]
    pub name: String,
    /// Increases with every change of content; see [`Index::check_serial`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<u64>,
    /// When the index was generated (RFC 3339, UTC). Informative only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<String>,
    pub plugins: Vec<Plugin>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// `lib` or `plugin`; absent means `plugin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Pre-selected by the installer.
    #[serde(default, skip_serializing_if = "is_false")]
    pub default: bool,
    /// A `lib` that a default plugin needs.
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    /// Published by the official maintainers.
    #[serde(default, skip_serializing_if = "is_false")]
    pub official: bool,
    pub versions: Vec<Release>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    /// Loader ABI version the release needs.
    #[serde(default = "default_api")]
    pub api: u32,
    /// Target platform. Missing values retain the original Windows meaning.
    #[serde(default, skip_serializing_if = "PlatformId::is_default")]
    pub platform: PlatformId,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// Ids the plugin needs installed first (its `requires`).
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    /// A withdrawn release: never chosen by default, but an exact `--version`
    /// still selects it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub yanked: bool,
    /// Other locations of the same package, tried in order after `url`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<String>,
}

fn default_api() -> u32 {
    1
}

fn is_false(value: &bool) -> bool {
    !*value
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
            if let Some(role) = plugin
                .role
                .as_deref()
                .filter(|role| !matches!(*role, "lib" | "plugin"))
            {
                return bad(format!(
                    "plugin {} has role {role:?}; expected \"lib\" or \"plugin\"",
                    plugin.id
                ));
            }
            let mut versions: Vec<(Version, PlatformId)> = Vec::new();
            for release in &plugin.versions {
                let version = Version::parse(&release.version).map_err(|error| {
                    Error::Index(format!(
                        "{} {:?} is not a semantic version: {error}",
                        plugin.id, release.version
                    ))
                })?;
                if versions.contains(&(version.clone(), release.platform.clone())) {
                    return bad(format!(
                        "{} {version} for {} is listed twice",
                        plugin.id,
                        release.platform.as_str()
                    ));
                }
                versions.push((version, release.platform.clone()));
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

    /// Refuses an index older than the one verified before (`cached` is that
    /// index's serial). A missing serial counts as 0, so an index without one
    /// is refused once a verified index with a serial has been cached.
    pub fn check_serial(&self, cached: Option<u64>) -> Result<(), Error> {
        let (serial, cached) = (self.serial.unwrap_or(0), cached.unwrap_or(0));
        if serial < cached {
            return Err(Error::Index(format!(
                "index serial {serial} is older than the verified serial {cached}: refusing a rollback"
            )));
        }
        Ok(())
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
        platform: &PlatformId,
    ) -> Result<&Release, Error> {
        if let Some(wanted) = version {
            let wanted = Version::parse(wanted)
                .map_err(|error| Error::Usage(format!("{wanted:?} is not a version: {error}")))?;
            let release = self
                .versions
                .iter()
                .find(|release| {
                    Version::parse(&release.version).ok().as_ref() == Some(&wanted)
                        && release.platform == *platform
                })
                .or_else(|| {
                    self.versions.iter().find(|release| {
                        Version::parse(&release.version).ok().as_ref() == Some(&wanted)
                    })
                })
                .ok_or_else(|| Error::NotFound(format!("{} has no version {wanted}", self.id)))?;
            if release.platform != *platform {
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
            .filter(|release| !release.yanked)
            .filter(|release| release.platform == *platform)
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
            platform: PlatformId::windows_x86_64(),
            url: format!("p-{version}.zip"),
            sha256: "ab".repeat(32),
            size: 10,
            requires: Vec::new(),
            published: None,
            yanked: false,
            mirrors: Vec::new(),
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
            ..Default::default()
        }
    }

    fn index_of(plugins: Vec<Plugin>) -> Vec<u8> {
        serde_json::to_vec(&Index {
            format: FORMAT,
            name: "test".into(),
            serial: None,
            generated: None,
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
            PlatformId::windows_x86_64()
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
    fn a_yanked_release_is_skipped_unless_its_version_is_asked_for() {
        let mut yanked = release("2.0.0", 1);
        yanked.yanked = true;
        let p = plugin(vec![release("1.0.0", 1), yanked]);
        let windows = PlatformId::windows_x86_64();
        assert_eq!(p.select(None, 1, &windows).unwrap().version, "1.0.0");
        assert_eq!(
            p.select(Some("2.0.0"), 1, &windows).unwrap().version,
            "2.0.0"
        );

        let mut only = release("1.0.0", 1);
        only.yanked = true;
        assert!(matches!(
            plugin(vec![only]).select(None, 1, &windows),
            Err(Error::Incompatible(_))
        ));
    }

    #[test]
    fn an_index_older_than_the_verified_one_is_refused() {
        let mut index = Index::parse(&index_of(vec![plugin(vec![release("1.0.0", 1)])])).unwrap();
        assert!(index.check_serial(None).is_ok(), "nothing cached");
        assert!(index.check_serial(Some(0)).is_ok(), "a missing serial is 0");
        index.serial = Some(5);
        assert!(index.check_serial(Some(5)).is_ok(), "the same index again");
        assert!(index.check_serial(Some(4)).is_ok(), "newer than the cache");
        let error = index.check_serial(Some(6)).unwrap_err();
        assert!(matches!(error, Error::Index(_)), "{error}");
        assert!(error.to_string().contains("rollback"), "{error}");
        index.serial = None;
        assert!(
            index.check_serial(Some(1)).is_err(),
            "a missing serial is 0"
        );
    }

    #[test]
    fn the_additive_fields_round_trip_and_stay_out_of_plain_indexes() {
        let text = format!(
            r#"{{"format":1,"serial":7,"generated":"2026-10-15T00:00:00Z","x-future":{{"a":1}},"plugins":[{{"id":"a","role":"lib","summary":"s","category":"c","homepage":"h","group":"g","default":true,"required":true,"official":true,"versions":[{{"version":"1.0.0","url":"a.zip","sha256":"{}","size":1,"published":"2026-10-01","yanked":true,"mirrors":["https://m.example/a.zip"]}}]}}]}}"#,
            "0".repeat(64)
        );
        let index = Index::parse(text.as_bytes()).unwrap();
        assert_eq!(index.serial, Some(7));
        let listed = &index.plugins[0];
        assert_eq!(listed.role.as_deref(), Some("lib"));
        assert!(listed.default && listed.required && listed.official);
        assert!(listed.versions[0].yanked);
        assert_eq!(listed.versions[0].mirrors, ["https://m.example/a.zip"]);
        assert_eq!(Index::parse(index.to_json().as_bytes()).unwrap(), index);

        let plain = Index::parse(&index_of(vec![plugin(vec![release("1.0.0", 1)])])).unwrap();
        let json = plain.to_json();
        for key in [
            "serial",
            "generated",
            "role",
            "summary",
            "default",
            "required",
            "official",
            "published",
            "yanked",
            "mirrors",
        ] {
            assert!(
                !json.contains(&format!("\"{key}\"")),
                "{key} is written: {json}"
            );
        }
    }

    #[test]
    fn a_role_is_lib_or_plugin() {
        for role in ["lib", "plugin"] {
            let mut p = plugin(vec![release("1.0.0", 1)]);
            p.role = Some(role.into());
            assert!(Index::parse(&index_of(vec![p])).is_ok(), "{role}");
        }
        let mut app = plugin(vec![release("1.0.0", 1)]);
        app.role = Some("app".into());
        let error = Index::parse(&index_of(vec![app])).unwrap_err();
        assert!(matches!(error, Error::Index(_)), "{error}");
        assert!(error.to_string().contains("role"), "{error}");
    }

    #[test]
    fn platform_ids_are_checked_as_os_and_architecture() {
        for good in [
            "windows-x86_64",
            "macos-arm64",
            "linux-x86_64",
            "x86-64",
            "a-b_c1",
        ] {
            let parsed = PlatformId::try_from(good.to_string()).unwrap();
            assert_eq!(parsed.as_str(), good);
        }
        for bad in [
            "",
            "windows",
            "-x86_64",
            "windows-",
            "Windows-x86_64",
            "windows-X86_64",
            "windows-x86-64",
            "windows_x86_64",
            "windows x86_64",
            "../windows-x86_64",
            "windows-x86_64 ",
            "windows-x86_64\n",
        ] {
            let error = PlatformId::try_from(bad.to_string()).unwrap_err();
            assert!(matches!(error, Error::Index(_)), "{bad:?}: {error}");
        }
    }

    #[test]
    fn a_missing_platform_is_windows_and_an_invalid_one_refuses_the_index() {
        assert_eq!(PlatformId::default(), PlatformId::windows_x86_64());
        let release = |platform: &str| {
            format!(
                r#"{{"format":1,"plugins":[{{"id":"a","versions":[{{"version":"1.0.0"{platform},"url":"a.zip","sha256":"{}","size":1}}]}}]}}"#,
                "0".repeat(64)
            )
        };
        let windows = Index::parse(release("").as_bytes()).unwrap();
        assert_eq!(
            windows.plugins[0].versions[0].platform,
            PlatformId::windows_x86_64()
        );
        let refused = Index::parse(release(r#","platform":"Windows""#).as_bytes());
        assert!(matches!(refused, Err(Error::Index(_))), "{refused:?}");
    }

    #[test]
    fn a_platform_this_tool_does_not_serve_is_read_but_never_selected() {
        let index = Index::parse(
            format!(
                r#"{{"format":1,"plugins":[{{"id":"a","versions":[{{"version":"2.0.0","platform":"linux-x86_64","url":"a.zip","sha256":"{0}","size":1}},{{"version":"1.0.0","url":"a.zip","sha256":"{0}","size":1}}]}}]}}"#,
                "0".repeat(64)
            )
            .as_bytes(),
        )
        .unwrap();
        let plugin = &index.plugins[0];
        let windows = PlatformId::windows_x86_64();
        assert_eq!(plugin.select(None, 1, &windows).unwrap().version, "1.0.0");
        assert!(matches!(
            plugin.select(Some("2.0.0"), 1, &windows),
            Err(Error::Incompatible(_))
        ));
        assert!(index.to_json().contains("\"platform\": \"linux-x86_64\""));
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
            p.select(None, 1, &PlatformId::windows_x86_64())
                .unwrap()
                .version,
            "1.10.0",
            "semver, not text order"
        );
        assert_eq!(
            p.select(None, 2, &PlatformId::windows_x86_64())
                .unwrap()
                .version,
            "2.0.0"
        );
    }

    #[test]
    fn mixed_platform_versions_are_filtered_and_old_indexes_stay_windows() {
        let mut windows = release("2.0.0", 1);
        windows.platform = PlatformId::windows_x86_64();
        let mut macos = release("1.5.0", 1);
        macos.platform = PlatformId::macos_arm64();
        let p = plugin(vec![windows, macos]);

        assert_eq!(
            p.select(None, 1, &PlatformId::windows_x86_64())
                .unwrap()
                .version,
            "2.0.0"
        );
        assert_eq!(
            p.select(None, 1, &PlatformId::macos_arm64())
                .unwrap()
                .version,
            "1.5.0"
        );
        assert!(matches!(
            p.select(Some("2.0.0"), 1, &PlatformId::macos_arm64()),
            Err(Error::Incompatible(_))
        ));

        let old = Index::parse(
            br#"{"format":1,"plugins":[{"id":"legacy","versions":[{"version":"1.0.0","url":"p.zip","sha256":"0000000000000000000000000000000000000000000000000000000000000000","size":1}]}]}"#,
        )
        .unwrap();
        assert_eq!(
            old.plugins[0].versions[0].platform,
            PlatformId::windows_x86_64()
        );
        assert!(!old.to_json().contains("platform"));
    }

    #[test]
    fn one_semver_can_have_a_package_for_each_platform() {
        let mut windows = release("1.0.0", 1);
        windows.platform = PlatformId::windows_x86_64();
        let mut macos = release("1.0.0", 1);
        macos.platform = PlatformId::macos_arm64();
        let parsed = Index::parse(&index_of(vec![plugin(vec![windows, macos])])).unwrap();
        let plugin = parsed.plugin("com.example.p").unwrap();

        assert_eq!(
            plugin
                .select(Some("1.0.0"), 1, &PlatformId::windows_x86_64())
                .unwrap()
                .platform,
            PlatformId::windows_x86_64()
        );
        assert_eq!(
            plugin
                .select(Some("1.0.0"), 1, &PlatformId::macos_arm64())
                .unwrap()
                .platform,
            PlatformId::macos_arm64()
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
            p.select(Some("1.0.0"), 1, &PlatformId::windows_x86_64())
                .unwrap()
                .version,
            "1.0.0"
        );
        assert_eq!(
            p.select(Some("3.0.0-rc.1"), 1, &PlatformId::windows_x86_64())
                .unwrap()
                .version,
            "3.0.0-rc.1"
        );
        assert!(matches!(
            p.select(Some("2.0.0"), 1, &PlatformId::windows_x86_64()),
            Err(Error::Incompatible(_))
        ));
        assert!(matches!(
            p.select(Some("9.9.9"), 1, &PlatformId::windows_x86_64()),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            p.select(Some("nope"), 1, &PlatformId::windows_x86_64()),
            Err(Error::Usage(_))
        ));
        let only_new = plugin(vec![release("2.0.0", 2)]);
        assert!(matches!(
            only_new.select(None, 1, &PlatformId::windows_x86_64()),
            Err(Error::Incompatible(_))
        ));
        let only_pre = plugin(vec![release("1.0.0-alpha", 1)]);
        assert!(
            only_pre
                .select(None, 1, &PlatformId::windows_x86_64())
                .is_err(),
            "no pre-release by default"
        );
    }
}
