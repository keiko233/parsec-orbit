//! SHA-256 digests and metadata snapshots used for provenance and restore
//! verification.
//!
//! A [`Tree`] records one entry per file, directory and symlink (relative
//! path, kind, permission bits, size, content hash or link target). Restore
//! and rollback checks compare these snapshots, so "matches hashes and
//! metadata" is measured, not assumed.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::platform::mode_bits;

/// SHA-256 of one file, lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

pub fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// One entry of a filesystem snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: EntryKind,
    pub mode: u32,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
}

/// Relative path -> entry, sorted by path.
pub type Tree = BTreeMap<String, Entry>;

/// Snapshots a directory tree without following symlinks.
pub fn tree(root: &Path) -> Result<Tree, String> {
    let mut entries = BTreeMap::new();
    walk(root, root, &mut entries)?;
    Ok(entries)
}

fn walk(root: &Path, current: &Path, entries: &mut Tree) -> Result<(), String> {
    let listing =
        fs::read_dir(current).map_err(|error| format!("{}: {error}", current.display()))?;
    for item in listing {
        let item = item.map_err(|error| format!("{}: {error}", current.display()))?;
        let path = item.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("{} is not below {}", path.display(), root.display()))?
            .to_string_lossy()
            .into_owned();
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            let target = fs::read_link(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?
                .to_string_lossy()
                .into_owned();
            entries.insert(
                relative,
                Entry {
                    kind: EntryKind::Symlink,
                    mode: 0o777,
                    size: 0,
                    sha256: None,
                    target: Some(target),
                },
            );
        } else if file_type.is_dir() {
            entries.insert(
                relative,
                Entry {
                    kind: EntryKind::Dir,
                    mode: mode_bits(&metadata),
                    size: 0,
                    sha256: None,
                    target: None,
                },
            );
            walk(root, &path, entries)?;
        } else if file_type.is_file() {
            entries.insert(
                relative,
                Entry {
                    kind: EntryKind::File,
                    mode: mode_bits(&metadata),
                    size: metadata.len(),
                    sha256: Some(sha256_file(&path)?),
                    target: None,
                },
            );
        } else {
            return Err(format!(
                "{} is not a file, directory or symlink",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Relative paths whose entries differ, as `path: reason` strings.
pub fn differences(expected: &Tree, actual: &Tree) -> Vec<String> {
    changed_paths(expected, actual)
        .into_iter()
        .map(|(path, reason)| format!("{path}: {reason}"))
        .collect()
}

/// Relative paths whose entries differ, with a short machine-checkable
/// reason (`missing`, `differs`, `unexpected`).
pub fn changed_paths(expected: &Tree, actual: &Tree) -> BTreeMap<String, &'static str> {
    let mut report = BTreeMap::new();
    for (path, entry) in expected {
        match actual.get(path) {
            None => {
                report.insert(path.clone(), "missing");
            }
            Some(found) if found != entry => {
                report.insert(path.clone(), "differs");
            }
            Some(_) => {}
        }
    }
    for path in actual.keys() {
        if !expected.contains_key(path) {
            report.insert(path.clone(), "unexpected");
        }
    }
    report
}

/// Convenience: the canonical path of an existing path.
pub fn canonical(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_core::testutil::TempDir;

    #[test]
    fn tree_records_content_and_metadata() {
        let scratch = TempDir::new("installer-digest");
        fs::create_dir(scratch.path().join("sub")).unwrap();
        fs::write(scratch.path().join("sub/a.txt"), b"hello").unwrap();
        let snapshot = tree(scratch.path()).unwrap();
        assert_eq!(snapshot["sub"].kind, EntryKind::Dir);
        assert_eq!(snapshot["sub/a.txt"].kind, EntryKind::File);
        assert_eq!(snapshot["sub/a.txt"].size, 5);
        assert_eq!(
            snapshot["sub/a.txt"].sha256.as_deref(),
            Some(
                sha256_file(&scratch.path().join("sub/a.txt"))
                    .unwrap()
                    .as_str()
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn tree_records_symlink_targets() {
        let scratch = TempDir::new("installer-digest-links");
        fs::create_dir(scratch.path().join("sub")).unwrap();
        fs::write(scratch.path().join("sub/a.txt"), b"hello").unwrap();
        std::os::unix::fs::symlink("sub/a.txt", scratch.path().join("link")).unwrap();
        let snapshot = tree(scratch.path()).unwrap();
        assert_eq!(snapshot["link"].kind, EntryKind::Symlink);
        assert_eq!(snapshot["link"].target.as_deref(), Some("sub/a.txt"));
    }

    #[test]
    fn differences_reports_missing_changed_and_unexpected() {
        let scratch = TempDir::new("installer-digest-diff");
        fs::write(scratch.path().join("a"), b"one").unwrap();
        let before = tree(scratch.path()).unwrap();
        fs::write(scratch.path().join("a"), b"two").unwrap();
        fs::write(scratch.path().join("b"), b"new").unwrap();
        let after = tree(scratch.path()).unwrap();
        let report = differences(&before, &after);
        assert!(report.iter().any(|line| line == "a: differs"), "{report:?}");
        assert!(
            report.iter().any(|line| line == "b: unexpected"),
            "{report:?}"
        );
    }
}
