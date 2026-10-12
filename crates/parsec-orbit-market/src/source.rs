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

/// Whether `location` names something on this machine: a `file://` URL, a
/// Windows drive path or a UNC share (`\\host\share` and `//host/share`).
fn is_local(location: &str) -> bool {
    location.starts_with("file://")
        || location.starts_with("\\\\")
        || location.starts_with("//")
        || Path::new(location).is_absolute()
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

/// `relative` as seen from `base`: absolute locations stay as they are, the
/// rest is looked up next to the file (or URL) `base`. An index fetched over
/// http(s) may only name http(s) locations: a path or UNC share in a remote
/// index would read the user's disk and, on Windows, hand credentials to the
/// share's host.
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
    if is_url(relative) || relative.starts_with("file://") || Path::new(relative).is_absolute() {
        return Ok(relative.to_string());
    }
    if relative.split(['/', '\\']).any(|part| part == "..") {
        return Err(Error::Index(format!(
            "{relative:?} climbs out of the index folder"
        )));
    }
    let parent = file_path(base)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    Ok(parent.join(relative).to_string_lossy().into_owned())
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
