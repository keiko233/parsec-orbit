//! Where bytes come from: files and https.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::Error;

/// Fetches the bytes behind a location (a path, a `file://` URL or an
/// `http(s)://` URL), refusing more than `limit` bytes.
pub trait Fetch {
    fn fetch(&self, location: &str, limit: u64) -> Result<Vec<u8>, Error>;
}

/// The real thing.
pub struct Real;

fn is_url(location: &str) -> bool {
    location.starts_with("http://") || location.starts_with("https://")
}

fn file_path(location: &str) -> PathBuf {
    let path = location.strip_prefix("file:///").unwrap_or(location);
    PathBuf::from(path)
}

/// Whether `location` starts with a Windows drive designator, `C:` (either
/// separator). Checked by text rather than through `Path`, which reads such a
/// location as a relative path on unix: a remote index naming one would then
/// slip past the "http(s) only" rule.
fn is_drive_path(location: &str) -> bool {
    let bytes = location.as_bytes();
    bytes
        .first()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && bytes.get(1) == Some(&b':')
        && matches!(bytes.get(2), Some(b'/') | Some(b'\\'))
}

/// Whether `location` names something on this machine: a `file://` URL, a
/// Windows drive path or a UNC share (`\\host\share` and `//host/share`).
fn is_local(location: &str) -> bool {
    location.starts_with("file://")
        || location.starts_with("\\\\")
        || location.starts_with("//")
        || is_drive_path(location)
        || Path::new(location).is_absolute()
}

/// Joins `relative` under `parent`, following the separator the base path
/// spells (`style`), so a Windows location stays a Windows location even when
/// the tool runs on unix. A base with no folder has no separator to follow:
/// the relative location is passed through as written.
fn join_under(parent: &str, relative: &str, style: &str) -> String {
    if parent.is_empty() {
        return relative.to_string();
    }
    let separator = if style.contains('\\') { '\\' } else { '/' };
    let relative = if separator == '\\' {
        relative.replace('/', "\\")
    } else {
        relative.to_string()
    };
    // Only drop the trailing separator when something is left of it: a root
    // such as `/` or `C:\` is the parent itself.
    let parent = if parent.len() > 1 {
        parent.trim_end_matches(separator)
    } else {
        parent
    };
    format!("{parent}{separator}{relative}")
}

impl Fetch for Real {
    fn fetch(&self, location: &str, limit: u64) -> Result<Vec<u8>, Error> {
        if !is_url(location) {
            let path = file_path(location);
            let mut bytes = Vec::new();
            std::fs::File::open(&path)
                .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?
                .take(limit + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > limit {
                return Err(Error::Io(format!(
                    "{} is larger than {limit} bytes",
                    path.display()
                )));
            }
            return Ok(bytes);
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .into();
        let mut response = agent
            .get(location)
            .call()
            .map_err(|error| Error::Network(format!("{location}: {error}")))?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| Error::Network(format!("{location}: {error}")))?;
        if bytes.len() as u64 > limit {
            return Err(Error::Network(format!(
                "{location} is larger than {limit} bytes"
            )));
        }
        Ok(bytes)
    }
}

/// Replaces the file at `path` with `bytes` through a temporary file, so a
/// reader sees the old content or the new, never a part of either.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let temporary = PathBuf::from(format!("{}.tmp", path.display()));
    if let Err(error) =
        std::fs::write(&temporary, bytes).and_then(|()| std::fs::rename(&temporary, path))
    {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

/// `relative` as seen from `base`: absolute locations stay as they are, the
/// rest is looked up next to the file (or URL) `base`. An index fetched over
/// http(s) may only name http(s) locations: a path or UNC share in a remote
/// index would read the user's disk and, on Windows, hand credentials to the
/// share's host. A Windows drive path or UNC share is treated as a local
/// location on every host - `Path` reads `C:\...` as a relative path on unix,
/// so the check is textual, and a remote index may not name one there either.
pub fn resolve(base: &str, relative: &str) -> Result<String, Error> {
    if is_url(base) {
        if is_url(relative) {
            return Ok(relative.to_string());
        }
        if is_local(relative) {
            return Err(Error::Index(format!(
                "{relative:?} in an index fetched over http(s) is not an http(s) location"
            )));
        }
        if relative.split(['/', '\\']).any(|part| part == "..") {
            return Err(Error::Index(format!(
                "{relative:?} climbs out of the index folder"
            )));
        }
        let cut = base.rfind('/').map_or(base.len(), |at| at + 1);
        return Ok(format!(
            "{}{}",
            &base[..cut],
            relative.trim_start_matches("./")
        ));
    }
    if is_url(relative) || is_local(relative) {
        return Ok(relative.to_string());
    }
    if relative.split(['/', '\\']).any(|part| part == "..") {
        return Err(Error::Index(format!(
            "{relative:?} climbs out of the index folder"
        )));
    }
    let at = base.rfind(['/', '\\']);
    let parent = at.map_or("", |at| &base[..at]);
    Ok(join_under(parent, relative, base))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Serves fixed bytes by location and records what was asked for.
    #[derive(Default)]
    pub struct Memory {
        pub files: HashMap<String, Vec<u8>>,
        pub asked: Mutex<Vec<String>>,
    }

    impl Fetch for Memory {
        fn fetch(&self, location: &str, limit: u64) -> Result<Vec<u8>, Error> {
            self.asked.lock().unwrap().push(location.to_string());
            let bytes = self
                .files
                .get(location)
                .ok_or_else(|| Error::Io(format!("{location}: not found")))?;
            if bytes.len() as u64 > limit {
                return Err(Error::Io(format!("{location} is too large")));
            }
            Ok(bytes.clone())
        }
    }

    #[test]
    fn relative_locations_are_resolved_next_to_the_index() {
        assert_eq!(
            resolve("https://example.com/orbit/index.json", "p.zip").unwrap(),
            "https://example.com/orbit/p.zip"
        );
        assert_eq!(
            resolve("https://example.com/orbit/index.json", "./sub/p.zip").unwrap(),
            "https://example.com/orbit/sub/p.zip"
        );
        assert_eq!(
            resolve(
                "https://example.com/orbit/index.json",
                "https://cdn.example/p.zip"
            )
            .unwrap(),
            "https://cdn.example/p.zip"
        );
        let local = resolve("C:\\market\\index.json", "p.zip").unwrap();
        assert!(
            local.ends_with("p.zip") && local.contains("market"),
            "{local}"
        );
    }

    #[test]
    fn a_location_cannot_climb_out_of_the_index_folder() {
        assert!(resolve("https://example.com/orbit/index.json", "../x.zip").is_err());
        assert!(resolve("index.json", "a/../../x.zip").is_err());
        assert!(resolve("index.json", "a\\..\\x.zip").is_err());
    }

    #[test]
    fn a_windows_location_keeps_its_separator_on_any_host() {
        // The folder part follows the base's separator, so a Windows index
        // still produces a Windows path when the tool runs on unix.
        assert_eq!(
            resolve("C:\\market\\index.json", "sub\\p.zip").unwrap(),
            "C:\\market\\sub\\p.zip"
        );
        assert_eq!(
            resolve("C:\\market\\index.json", "sub/p.zip").unwrap(),
            "C:\\market\\sub\\p.zip"
        );
        // A bare file name has no folder to follow: it is relative to the
        // working directory and stays as written.
        assert_eq!(resolve("index.json", "p.zip").unwrap(), "p.zip");
        // A drive root stays a root.
        assert_eq!(resolve("C:\\index.json", "p.zip").unwrap(), "C:\\p.zip");
        // A drive path is local on every host, so a remote index may not name
        // one - `Path` reads it as relative on unix.
        assert!(resolve("https://example.com/orbit/index.json", "C:/market/p.zip").is_err());
    }

    #[test]
    fn a_remote_index_can_only_name_remote_locations() {
        for local in [
            "C:\\market\\p.zip",
            "\\\\host\\share\\p.zip",
            "//host/share/p.zip",
            "file:///C:/market/p.zip",
        ] {
            assert!(
                resolve("https://example.com/orbit/index.json", local).is_err(),
                "{local} was accepted"
            );
        }
        // The same locations are fine when the index itself is a local file:
        // that is how a folder of zips is installed.
        for local in ["C:\\market\\p.zip", "\\\\host\\share\\p.zip"] {
            assert_eq!(
                resolve("C:\\market\\index.json", local).unwrap(),
                local,
                "{local} was refused for a local index"
            );
        }
    }

    #[test]
    fn files_are_read_up_to_the_limit() {
        let path =
            std::env::temp_dir().join(format!("parsec-orbit-fetch-{}.bin", std::process::id()));
        std::fs::write(&path, b"12345").unwrap();
        let location = path.to_string_lossy().into_owned();
        assert_eq!(Real.fetch(&location, 5).unwrap(), b"12345");
        assert!(Real.fetch(&location, 4).is_err());
        assert!(Real.fetch("C:\\no\\such\\file.bin", 10).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_network_is_reached_for_urls_only() {
        // Port 1 refuses connections; the error says it was the network.
        let error = Real.fetch("http://127.0.0.1:1/index.json", 10).unwrap_err();
        assert!(matches!(error, Error::Network(_)), "{error}");
    }
}
