//! The loader and installer of each release: `host.json` (format 1), the
//! generated document `orbit upgrade` reads. Its layout is section 4.3 of
//! `docs/design/restructure.md`.
//!
//! ```json
//! {
//!   "format": 1,
//!   "serial": 3,
//!   "latest": "0.2.0",
//!   "releases": [{
//!     "version": "0.2.0",
//!     "api": 1,
//!     "platform": "windows-x86_64",
//!     "loader": { "url": "bcrypt.dll", "sha256": "<64 hex digits>", "size": 374272 },
//!     "installer": { "url": "orbit.exe", "sha256": "<64 hex digits>", "size": 1234 }
//!   }]
//! }
//! ```
//!
//! A location is `https://` or relative to the document, without `..`. The
//! whole document is signed the same way as the plugin index.

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::index::{is_sha256_hex, PlatformId, MAX_PACKAGE_BYTES};
use crate::Error;

/// The only `format` this version understands.
pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostManifest {
    pub format: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<String>,
    /// The newest version; it must be one of the releases.
    pub latest: String,
    pub releases: Vec<HostRelease>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRelease {
    pub version: String,
    /// Loader ABI version the release needs.
    pub api: u32,
    pub platform: PlatformId,
    pub loader: Artifact,
    pub installer: Artifact,
}

/// A file the host installs, with the digest and size it must have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

impl HostManifest {
    /// Parses and checks a `host.json`.
    pub fn parse(bytes: &[u8]) -> Result<HostManifest, Error> {
        let manifest: HostManifest = serde_json::from_slice(bytes)
            .map_err(|error| Error::Index(format!("not a valid host.json: {error}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), Error> {
        let bad = |message: String| Err(Error::Index(message));
        if self.format != FORMAT {
            return bad(format!(
                "host.json format {} is not supported (this tool reads {FORMAT})",
                self.format
            ));
        }
        if let Err(error) = Version::parse(&self.latest) {
            return bad(format!(
                "latest {:?} is not a semantic version: {error}",
                self.latest
            ));
        }
        let mut seen: Vec<(Version, PlatformId)> = Vec::new();
        for release in &self.releases {
            let version = Version::parse(&release.version).map_err(|error| {
                Error::Index(format!(
                    "{:?} is not a semantic version: {error}",
                    release.version
                ))
            })?;
            let key = (version.clone(), release.platform.clone());
            if seen.contains(&key) {
                return bad(format!(
                    "{version} for {} is listed twice",
                    release.platform.as_str()
                ));
            }
            seen.push(key);
            release.loader.validate(&format!("{version} loader"))?;
            release
                .installer
                .validate(&format!("{version} installer"))?;
        }
        if !self
            .releases
            .iter()
            .any(|release| release.version == self.latest)
        {
            return bad(format!("latest {} is not one of the releases", self.latest));
        }
        Ok(())
    }

    /// The release for `platform` that the loader API `api` can run: exactly
    /// `version` if one is asked for, else the newest that is not a pre-release.
    pub fn select(
        &self,
        version: Option<&str>,
        platform: &PlatformId,
        api: u32,
    ) -> Result<&HostRelease, Error> {
        if let Some(wanted) = version {
            let wanted = Version::parse(wanted)
                .map_err(|error| Error::Usage(format!("{wanted:?} is not a version: {error}")))?;
            let release = self
                .releases
                .iter()
                .find(|release| {
                    &release.platform == platform
                        && Version::parse(&release.version).ok().as_ref() == Some(&wanted)
                })
                .ok_or_else(|| Error::NotFound(format!("no {wanted} for {}", platform.as_str())))?;
            if release.api > api {
                return Err(Error::Incompatible(format!(
                    "{wanted} needs loader API {} but this one implements {api}",
                    release.api
                )));
            }
            return Ok(release);
        }
        self.releases
            .iter()
            .filter(|release| &release.platform == platform && release.api <= api)
            .filter_map(|release| Some((Version::parse(&release.version).ok()?, release)))
            .filter(|(version, _)| version.pre.is_empty())
            .max_by(|(a, _), (b, _)| a.cmp(b))
            .map(|(_, release)| release)
            .ok_or_else(|| {
                Error::Incompatible(format!(
                    "no release for {} and loader API {api}",
                    platform.as_str()
                ))
            })
    }
}

impl Artifact {
    fn validate(&self, what: &str) -> Result<(), Error> {
        if !is_sha256_hex(&self.sha256) {
            return Err(Error::Index(format!(
                "{what}: sha256 must be 64 lowercase hex digits"
            )));
        }
        if self.size == 0 || self.size > MAX_PACKAGE_BYTES {
            return Err(Error::Index(format!(
                "{what}: size {} is outside 1..={MAX_PACKAGE_BYTES}",
                self.size
            )));
        }
        if !safe_location(&self.url) {
            return Err(Error::Index(format!(
                "{what}: url {:?} must be https:// or relative, without `..`",
                self.url
            )));
        }
        Ok(())
    }
}

/// An `https://` location, or a relative one that stays in its folder: no
/// drive letter, no scheme, no root, no `..`.
fn safe_location(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    !url.is_empty()
        && !url.contains(':')
        && !url.starts_with(['/', '\\'])
        && url.split(['/', '\\']).all(|part| part != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(url: &str, sha256: &str, size: u64) -> String {
        format!(r#"{{"url":"{url}","sha256":"{sha256}","size":{size}}}"#)
    }

    fn release(version: &str, platform: &str, api: u32) -> String {
        let good = artifact("bcrypt.dll", &"ab".repeat(32), 10);
        format!(
            r#"{{"version":"{version}","api":{api},"platform":"{platform}","loader":{good},"installer":{good}}}"#
        )
    }

    fn document(releases: &[String], latest: &str) -> String {
        format!(
            r#"{{"format":1,"serial":3,"x-note":"unknown keys are ignored","latest":"{latest}","releases":[{}]}}"#,
            releases.join(",")
        )
    }

    #[test]
    fn a_release_is_selected_by_platform_api_and_version() {
        let text = document(
            &[
                release("0.1.0", "windows-x86_64", 1),
                release("0.2.0", "windows-x86_64", 1),
                release("0.3.0", "windows-x86_64", 2),
                release("0.2.0", "macos-arm64", 1),
                release("0.3.0-rc.1", "windows-x86_64", 1),
            ],
            "0.3.0",
        );
        let host = HostManifest::parse(text.as_bytes()).unwrap();
        assert_eq!(host.serial, Some(3));
        let windows = PlatformId::windows_x86_64();
        let macos = PlatformId::macos_arm64();

        // 0.3.0 needs API 2 and the release candidate is a pre-release.
        assert_eq!(host.select(None, &windows, 1).unwrap().version, "0.2.0");
        assert_eq!(host.select(None, &windows, 2).unwrap().version, "0.3.0");
        assert_eq!(host.select(None, &macos, 1).unwrap().version, "0.2.0");
        assert_eq!(
            host.select(Some("0.3.0-rc.1"), &windows, 1)
                .unwrap()
                .version,
            "0.3.0-rc.1"
        );
        assert!(matches!(
            host.select(Some("0.3.0"), &windows, 1),
            Err(Error::Incompatible(_))
        ));
        assert!(matches!(
            host.select(Some("9.9.9"), &windows, 1),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            host.select(Some("one"), &windows, 1),
            Err(Error::Usage(_))
        ));
        let linux = PlatformId::try_from("linux-x86_64".to_string()).unwrap();
        assert!(matches!(
            host.select(None, &linux, 1),
            Err(Error::Incompatible(_))
        ));
    }

    #[test]
    fn a_document_that_breaks_a_rule_is_refused() {
        let refuse = |releases: String, latest: &str| {
            let error = HostManifest::parse(document(&[releases], latest).as_bytes()).unwrap_err();
            assert!(matches!(error, Error::Index(_)), "{error}");
        };
        let good = artifact("bcrypt.dll", &"ab".repeat(32), 10);
        let one = |loader: &str| {
            format!(
                r#"{{"version":"1.0.0","api":1,"platform":"windows-x86_64","loader":{loader},"installer":{good}}}"#
            )
        };
        HostManifest::parse(document(&[one(&good)], "1.0.0").as_bytes()).unwrap();

        refuse(one(&artifact("bcrypt.dll", &"AB".repeat(32), 10)), "1.0.0");
        refuse(one(&artifact("bcrypt.dll", "abc", 10)), "1.0.0");
        refuse(one(&artifact("bcrypt.dll", &"ab".repeat(32), 0)), "1.0.0");
        refuse(
            one(&artifact(
                "bcrypt.dll",
                &"ab".repeat(32),
                MAX_PACKAGE_BYTES + 1,
            )),
            "1.0.0",
        );
        for url in [
            "http://example.com/bcrypt.dll",
            "file:///C:/bcrypt.dll",
            "C:\\bcrypt.dll",
            r"\\host\share\bcrypt.dll",
            "/etc/bcrypt.dll",
            "../bcrypt.dll",
            "dir/../../bcrypt.dll",
            "",
        ] {
            refuse(one(&artifact(url, &"ab".repeat(32), 10)), "1.0.0");
        }
        refuse(
            one(&good).replace(
                "\"platform\":\"windows-x86_64\"",
                "\"platform\":\"Windows\"",
            ),
            "1.0.0",
        );
        refuse(
            one(&good).replace("\"version\":\"1.0.0\"", "\"version\":\"one\""),
            "one",
        );
        refuse(one(&good), "9.9.9");
        refuse(format!("{},{}", one(&good), one(&good)), "1.0.0");
        let text = document(&[one(&good)], "1.0.0").replace("\"format\":1", "\"format\":2");
        assert!(HostManifest::parse(text.as_bytes()).is_err());
        assert!(HostManifest::parse(b"[]").is_err());
    }
}
