//! The sources of `manifest/`: `plugins/<id>/entry.toml`,
//! `plugins/<id>/releases.toml` and `host/releases.toml`, read with serde.
//! Nothing here writes them back: a release only appends text
//! (`add.rs`), so hand-written content is never re-serialised.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use parsec_orbit_registry::index::{is_sha256_hex, PlatformId, MAX_PACKAGE_BYTES};
use parsec_orbit_registry::Error;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use toml::value::Datetime;

/// Platform of a release that names none.
pub const DEFAULT_PLATFORM: &str = PlatformId::WINDOWS_X86_64;
/// Ids with this prefix belong to official modules, which have a `[build]`.
pub const RESERVED_PREFIX: &str = "com.parsec-orbit.";
/// Longest `summary`, in characters.
pub const SUMMARY_LIMIT: usize = 120;

#[derive(Debug, Deserialize)]
pub struct Entry {
    pub id: String,
    #[serde(default)]
    pub default: bool,
    pub group: Option<String>,
    pub category: Option<String>,
    pub summary: Option<String>,
    pub homepage: Option<String>,
    pub build: Option<Build>,
}

/// An official module that CI builds from this repository.
#[derive(Debug, Deserialize)]
pub struct Build {
    #[serde(default)]
    pub platforms: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PluginReleases {
    #[serde(default)]
    pub release: Vec<PluginRelease>,
}

#[derive(Debug, Deserialize)]
pub struct PluginRelease {
    pub version: String,
    pub platform: Option<String>,
    pub url: String,
    #[serde(default)]
    pub mirrors: Vec<String>,
    pub sha256: String,
    pub published: Option<Datetime>,
    #[serde(default)]
    pub yanked: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct HostReleases {
    #[serde(default)]
    pub release: Vec<HostRelease>,
}

#[derive(Debug, Deserialize)]
pub struct HostRelease {
    pub version: String,
    pub api: u32,
    pub platform: Option<String>,
    pub loader: Artifact,
    pub installer: Artifact,
}

/// A file the host installs: where it is, its digest and its size.
#[derive(Debug, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// The subfolders of `dir` that are not hidden, sorted; none when `dir` does
/// not exist.
pub fn sorted_dirs(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Error::Io(format!("{}: {error}", dir.display()))),
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| {
        let hidden = path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'));
        path.is_dir() && !hidden
    });
    paths.sort();
    Ok(paths)
}

/// Reads a TOML file; `None` when it does not exist.
pub fn read_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, Error> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::Io(format!("{}: {error}", path.display()))),
    };
    toml::from_str(&text)
        .map(Some)
        .map_err(|error| Error::Index(format!("{}: {error}", path.display())))
}

/// The platform a release names, or the default when it names none.
pub fn platform_of(platform: Option<&str>) -> Result<PlatformId, Error> {
    PlatformId::try_from(platform.unwrap_or(DEFAULT_PLATFORM).to_string())
}

/// Whether a location is `https://`, or relative to the document with no
/// scheme, drive, root or `..` (`docs/spec/registry.md`).
pub fn location_ok(location: &str) -> bool {
    if location.starts_with("https://") {
        return location.len() > "https://".len();
    }
    !location.is_empty()
        && !location.contains(':')
        && !location.starts_with(['/', '\\'])
        && !location.split(['/', '\\']).any(|part| part == "..")
}

/// Whether a TOML value is a bare date (`2026-11-01`).
pub fn is_date(value: &Datetime) -> bool {
    value.date.is_some() && value.time.is_none() && value.offset.is_none()
}

/// Checks one `[url, sha256, size]` artifact, for `what` in the message.
pub fn artifact_problems(artifact: &Artifact, what: &str) -> Vec<String> {
    let mut problems = Vec::new();
    if !location_ok(&artifact.url) {
        problems.push(format!(
            "{what} url must be https:// or a relative path without `..`"
        ));
    }
    if !is_sha256_hex(&artifact.sha256) {
        problems.push(format!("{what} sha256 must be 64 lowercase hex digits"));
    }
    if artifact.size == 0 || artifact.size > MAX_PACKAGE_BYTES {
        problems.push(format!(
            "{what} size {} is outside 1..={MAX_PACKAGE_BYTES}",
            artifact.size
        ));
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_are_https_or_relative_without_parent_steps() {
        for good in ["https://example.com/a.zip", "a.zip", "dir/a.zip"] {
            assert!(location_ok(good), "{good}");
        }
        for bad in [
            "http://example.com/a.zip",
            "file:///tmp/a.zip",
            "C:/a.zip",
            "/abs/a.zip",
            "../a.zip",
            "dir\\..\\a.zip",
            "",
            "https://",
        ] {
            assert!(!location_ok(bad), "{bad}");
        }
    }

    #[test]
    fn a_missing_platform_is_the_windows_default() {
        assert_eq!(platform_of(None).unwrap().as_str(), "windows-x86_64");
        assert_eq!(
            platform_of(Some("macos-arm64")).unwrap().as_str(),
            "macos-arm64"
        );
        assert!(platform_of(Some("Windows")).is_err());
    }

    #[test]
    fn only_a_bare_date_counts_as_a_date() {
        let date: Datetime = "2026-11-01".parse().unwrap();
        let stamp: Datetime = "2026-11-01T10:00:00Z".parse().unwrap();
        assert!(is_date(&date));
        assert!(!is_date(&stamp));
    }
}
