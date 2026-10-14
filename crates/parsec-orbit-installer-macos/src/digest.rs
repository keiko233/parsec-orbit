//! SHA-256 digests and metadata snapshots used for provenance and restore
//! verification.
//!
//! A [`Tree`] records one entry per file, directory and symlink (relative
//! path, kind, permission bits, size, content hash or link target). Restore
//! and rollback checks compare these snapshots, including the metadata captured
//! by [`MetadataSnapshot`].

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::platform::mode_bits;

#[path = "metadata.rs"]
mod metadata;
pub use metadata::{ExtendedAttribute, FileTimestamp, MetadataSnapshot};

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
    /// Optional filesystem metadata added after the initial manifest format.
    /// Missing values keep snapshots written by older installer versions
    /// readable and comparable using the fields they recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MetadataSnapshot>,
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

/// Relative path -> entry, sorted by path. The empty path (`""`) represents
/// the root directory itself.
pub type Tree = BTreeMap<String, Entry>;

/// Snapshots a directory tree without following symlinks.
pub fn tree(root: &Path) -> Result<Tree, String> {
    let mut entries = BTreeMap::new();
    let root_metadata =
        fs::symlink_metadata(root).map_err(|error| format!("{}: {error}", root.display()))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(format!("{} is not a real directory", root.display()));
    }
    entries.insert(
        String::new(),
        Entry {
            kind: EntryKind::Dir,
            mode: mode_bits(&root_metadata),
            size: 0,
            metadata: metadata::capture(root, &root_metadata)?,
            sha256: None,
            target: None,
        },
    );
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
                    metadata: metadata::capture(&path, &metadata)?,
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
                    metadata: metadata::capture(&path, &metadata)?,
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
                    metadata: metadata::capture(&path, &metadata)?,
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
    format_changes(changed_paths_impl(
        expected,
        actual,
        MetadataComparison::Exact,
    ))
}

/// Differences between a pristine tree and its macOS `ditto` staging copy.
/// `ditto` resets symlink modification times on macOS, so this comparison
/// permits that one copy transformation while checking all other metadata.
pub fn staged_copy_differences(expected: &Tree, actual: &Tree) -> Vec<String> {
    format_changes(changed_paths_impl(
        expected,
        actual,
        MetadataComparison::DittoCopy,
    ))
}

fn format_changes(changes: BTreeMap<String, &'static str>) -> Vec<String> {
    changes
        .into_iter()
        .map(|(path, reason)| format!("{path}: {reason}"))
        .collect()
}

/// Legacy path comparison over the original tree fields. New post-signing
/// checks should use [`changed_paths_for_signing`] so optional metadata on
/// payload files remains part of the allowlist decision.
pub fn changed_paths(expected: &Tree, actual: &Tree) -> BTreeMap<String, &'static str> {
    changed_paths_impl(expected, actual, MetadataComparison::Legacy)
}

/// Compares a signed staged tree against the pristine tree for the existing
/// changed-path allowlist. Symlink mtimes are excluded because `ditto` resets
/// them on macOS. Directory mtimes are excluded only for the explicitly named
/// entries; all other metadata, including payload file mtimes, owners, ACLs
/// and xattrs, remains part of the comparison.
pub fn changed_paths_for_signing(
    expected: &Tree,
    actual: &Tree,
    directory_mtime_exceptions: &[&str],
) -> BTreeMap<String, &'static str> {
    changed_paths_impl(
        expected,
        actual,
        MetadataComparison::Signing(directory_mtime_exceptions),
    )
}

fn changed_paths_impl(
    expected: &Tree,
    actual: &Tree,
    metadata_comparison: MetadataComparison<'_>,
) -> BTreeMap<String, &'static str> {
    let mut report = BTreeMap::new();
    for (path, entry) in expected {
        match actual.get(path) {
            None => {
                report.insert(path.clone(), "missing");
            }
            Some(found) if !entries_match(entry, found, path, metadata_comparison) => {
                report.insert(path.clone(), "differs");
            }
            Some(_) => {}
        }
    }
    for path in actual.keys() {
        // The root entry was added with metadata snapshots. Older manifests
        // omitted it, so its appearance alone must not invalidate those
        // records.
        if !expected.contains_key(path) && !path.is_empty() {
            report.insert(path.clone(), "unexpected");
        }
    }
    report
}

fn entries_match(
    expected: &Entry,
    actual: &Entry,
    path: &str,
    metadata_comparison: MetadataComparison<'_>,
) -> bool {
    expected.kind == actual.kind
        && expected.mode == actual.mode
        && expected.size == actual.size
        && expected.sha256 == actual.sha256
        && expected.target == actual.target
        && metadata_matches(expected, actual, path, metadata_comparison)
}

#[derive(Clone, Copy)]
enum MetadataComparison<'a> {
    Exact,
    DittoCopy,
    Signing(&'a [&'a str]),
    Legacy,
}

fn metadata_matches(
    expected: &Entry,
    actual: &Entry,
    path: &str,
    comparison: MetadataComparison<'_>,
) -> bool {
    if matches!(comparison, MetadataComparison::Legacy) {
        return true;
    }
    let Some(expected_metadata) = expected.metadata.as_ref() else {
        // Old manifests only recorded the pre-metadata fields.
        return true;
    };
    let Some(actual_metadata) = actual.metadata.as_ref() else {
        return false;
    };
    let ignore_mtime = match comparison {
        MetadataComparison::Exact | MetadataComparison::Legacy => false,
        MetadataComparison::DittoCopy => expected.kind == EntryKind::Symlink,
        MetadataComparison::Signing(directory_exceptions) => {
            expected.kind == EntryKind::Symlink
                || (expected.kind == EntryKind::Dir && directory_exceptions.contains(&path))
        }
    };
    if ignore_mtime {
        expected_metadata.uid == actual_metadata.uid
            && expected_metadata.gid == actual_metadata.gid
            && expected_metadata.xattrs == actual_metadata.xattrs
            && expected_metadata.acl == actual_metadata.acl
            && (expected.kind != EntryKind::File
                || expected_metadata.mtime == actual_metadata.mtime)
    } else {
        expected_metadata == actual_metadata
    }
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

    #[test]
    fn old_entries_deserialize_and_compare_without_new_metadata() {
        let old: Entry = serde_json::from_value(serde_json::json!({
            "kind": "file",
            "mode": 0o644,
            "size": 3,
            "sha256": "abc",
            "target": null
        }))
        .unwrap();
        assert!(old.metadata.is_none());

        let mut current = old.clone();
        current.metadata = Some(MetadataSnapshot::default());
        let expected = BTreeMap::from([("file".to_string(), old)]);
        let actual = BTreeMap::from([
            (
                String::new(),
                Entry {
                    kind: EntryKind::Dir,
                    mode: 0o755,
                    size: 0,
                    metadata: Some(MetadataSnapshot::default()),
                    sha256: None,
                    target: None,
                },
            ),
            ("file".to_string(), current),
        ]);

        assert!(differences(&expected, &actual).is_empty());
        assert!(changed_paths(&expected, &actual).is_empty());
    }

    #[test]
    fn signing_path_check_ignores_only_named_directory_mtimes() {
        let scratch = TempDir::new("installer-digest-metadata");
        fs::create_dir(scratch.path().join("sub")).unwrap();
        fs::write(scratch.path().join("file"), b"content").unwrap();
        let expected = tree(scratch.path()).unwrap();
        let mut actual = expected.clone();
        let changed_time = Some(FileTimestamp {
            seconds: 1,
            nanoseconds: 2,
        });
        actual.get_mut("").unwrap().metadata.as_mut().unwrap().mtime = changed_time;
        actual
            .get_mut("sub")
            .unwrap()
            .metadata
            .as_mut()
            .unwrap()
            .mtime = changed_time;
        actual
            .get_mut("file")
            .unwrap()
            .metadata
            .as_mut()
            .unwrap()
            .mtime = changed_time;

        assert_eq!(
            differences(&expected, &actual),
            [": differs", "file: differs", "sub: differs"]
        );
        assert!(changed_paths(&expected, &actual).is_empty());
        let signing = changed_paths_for_signing(&expected, &actual, &["", "sub"]);
        assert_eq!(signing, BTreeMap::from([("file".into(), "differs")]));
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
