//! The last verified index of each source, kept so that a later download
//! cannot roll the registry back (see [`crate::index::Index::check_serial`]).
//!
//! Each source gets a folder `<dir>/<sha256 of its location>/` holding
//! `index.json`, `index.json.sig` and `etag`. `etag` is the sha256 of
//! `index.json`: the transport does not expose response headers, so the digest
//! stands in for the ETag a conditional request would compare. Only an index
//! that a trusted key signed is ever stored, and each file is replaced by
//! writing a temporary file and renaming it over the old one.

use std::path::PathBuf;

use crate::source::write_atomic;
use crate::trust;
use crate::Error;

const INDEX: &str = "index.json";
const SIGNATURE: &str = "index.json.sig";
const ETAG: &str = "etag";

/// Cache of verified indexes, below one folder.
#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

/// The files of one cached index, as they were stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedIndex {
    pub index: Vec<u8>,
    pub signature: String,
    pub etag: String,
}

impl Cache {
    pub fn new(dir: PathBuf) -> Cache {
        Cache { dir }
    }

    fn source_dir(&self, source: &str) -> PathBuf {
        self.dir.join(trust::sha256_hex(source.as_bytes()))
    }

    /// The cached files of `source`, or `None` unless all of them can be read.
    pub fn load(&self, source: &str) -> Option<CachedIndex> {
        let dir = self.source_dir(source);
        Some(CachedIndex {
            index: std::fs::read(dir.join(INDEX)).ok()?,
            signature: std::fs::read_to_string(dir.join(SIGNATURE)).ok()?,
            etag: std::fs::read_to_string(dir.join(ETAG)).ok()?,
        })
    }

    /// Stores an index whose signature a trusted key has checked.
    pub fn store(&self, source: &str, index: &[u8], signature: &str) -> Result<(), Error> {
        let dir = self.source_dir(source);
        std::fs::create_dir_all(&dir)?;
        write_atomic(&dir.join(INDEX), index)?;
        write_atomic(&dir.join(SIGNATURE), signature.as_bytes())?;
        write_atomic(&dir.join(ETAG), trust::sha256_hex(index).as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Dir {
        fn new(label: &str) -> Dir {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "parsec-orbit-cache-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&path);
            Dir(path)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_stored_index_is_loaded_back_with_its_signature_and_etag() {
        let dir = Dir::new("roundtrip");
        let cache = Cache::new(dir.0.join("cache"));
        let source = "https://market.example/index.json";
        assert!(cache.load(source).is_none(), "nothing stored yet");

        cache.store(source, b"{\"format\":1}", "sig\n").unwrap();
        let loaded = cache.load(source).unwrap();
        assert_eq!(loaded.index, b"{\"format\":1}");
        assert_eq!(loaded.signature, "sig\n");
        assert_eq!(loaded.etag, trust::sha256_hex(b"{\"format\":1}"));

        // Another source has its own folder.
        assert!(cache.load("https://other.example/index.json").is_none());
    }

    #[test]
    fn a_second_store_replaces_the_first_without_leftovers() {
        let dir = Dir::new("replace");
        let cache = Cache::new(dir.0.clone());
        let source = "C:\\market\\index.json";
        cache.store(source, b"one", "a").unwrap();
        cache.store(source, b"two", "b").unwrap();
        let loaded = cache.load(source).unwrap();
        assert_eq!(
            (loaded.index.as_slice(), loaded.signature.as_str()),
            (&b"two"[..], "b")
        );

        let folder = cache.source_dir(source);
        let names: Vec<String> = std::fs::read_dir(&folder)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names.iter().all(|name| !name.ends_with(".tmp")));
    }

    #[test]
    fn a_partly_written_source_is_not_loaded() {
        let dir = Dir::new("partial");
        let cache = Cache::new(dir.0.clone());
        let source = "https://market.example/index.json";
        cache.store(source, b"index", "sig").unwrap();
        std::fs::remove_file(cache.source_dir(source).join(ETAG)).unwrap();
        assert!(cache.load(source).is_none());
    }
}
